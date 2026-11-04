//! The merge gates that used to live in the Python merge command, ported so
//! `authorized_merge::decide` is the one merge decision. Each gate
//! evaluates one input and answers `Option<Blocker>`: None clears. The pure
//! halves take their facts as arguments so unit tests need no filesystem or
//! network; the fetching halves ride the caller's `Probes` handle.

use crate::authorized_merge::{
    node_carries_tag, pr_bound_entry, Blocker, PrFacts, ProbeOutcome, Probes, MAIN_REPAIR_TAG,
};
use crate::loopcheck::coverage_gate::{refusal_line, GateState};
use crate::visual_screen;
use serde_json::Value;
use std::collections::BTreeSet;
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
/// `dep=contract` node this PR closes, or None. PR numbers are per-repo and
/// the graph store is global, so a ref whose pr_url names another repo never
/// answers (the shared-number specimen the ledger leg fixed); a ref with no
/// url, or one naming this checkout's remote, does. The graph read degrades
/// to None (the default hard merge path), exactly as the Python `dep` did.
pub(crate) fn contract_node_for_pr(entries: &[Value], pr: u64, slug: &str) -> Option<String> {
    for entry in entries {
        let dep = entry.get("dep").and_then(Value::as_str);
        if dep != Some("contract") {
            continue;
        }
        let answers = pr_refs_of(entry).iter().any(|r| {
            r.number == pr
                && match r.url.as_deref() {
                    None => true,
                    Some(url) => pr_url_names_this_repo(url, slug),
                }
        });
        if answers {
            return entry.get("id").and_then(Value::as_str).map(str::to_owned);
        }
    }
    None
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
    let slug = origin_slug(root);
    contract_node_for_pr(entries, pr, &slug).and_then(|node| stub_manifest_blocker(root, &node))
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
    // The gate computes in process now (the `fno do plan fidelity` shell hop
    // is retired with its Python leg). Fail-open polarity unchanged: only an
    // explicit refused decision blocks.
    let decision = crate::loopcheck::plan_fidelity::compute_plan_fidelity(plan_path, cwd, 180);
    if decision.get("refused").and_then(Value::as_bool) == Some(true) {
        let reason = decision
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

/// The plan_path bound to this PR's delivery row in the ledger, or None.
/// PR numbers are per-repo and the ledger is global, so when several rows
/// match, the one whose pr_url names this checkout's remote wins; a row
/// with no pr_url answers next. A row whose pr_url names another repo never
/// answers (the fnh/fno-web PR 70 specimen: another repo's plan refused a
/// merge here). A missing or broken ledger is None: no signal, no gate.
/// Whether a pr_url names this checkout's remote. GitHub spells the pair as
/// the path segment before `/pull/`, so the match is segment-tight: a
/// prefix-sibling repo (`org/fno` vs `org/fno-web`) never contains-matches.
fn pr_url_names_this_repo(url: &str, slug: &str) -> bool {
    !slug.is_empty() && url.contains(&format!("/{slug}/pull/"))
}

/// The `owner/name` slug this checkout's origin names, empty when
/// unresolvable. One shell so the ledger leg and the graph leg of the
/// merge-gate scoping cannot disagree about which remote answered.
fn origin_slug(cwd: &Path) -> String {
    use std::process::Command;

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
    repo_slug_from_origin(&origin).unwrap_or_default()
}

fn ledger_plan_path(cwd: &Path, pr: u64) -> Option<String> {
    // Read-only: the ledger is global (the Python writer appends to the
    // same file), so an undeclared test process answers None, the same
    // no-signal shape as a missing ledger, instead of reading the
    // operator's history.
    if cfg!(test) && !crate::paths::test_root_declared() {
        return None;
    }
    let rows = crate::loopcheck::ledger_file_rows(&crate::paths_cli::ledger_json(cwd));
    if rows.is_empty() {
        return None;
    }
    let slug = origin_slug(cwd);
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|row| row.get("pr_number").and_then(Value::as_u64) == Some(pr))
        .collect();
    let picked = matches
        .iter()
        .copied()
        .find(|row| {
            row.get("pr_url")
                .and_then(Value::as_str)
                .is_some_and(|url| pr_url_names_this_repo(url, &slug))
        })
        .or_else(|| {
            // An unmarked row keeps the first-match fail-open shape; a row
            // that names another repo never does.
            matches.iter().copied().find(|row| {
                row.get("pr_url")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            })
        });
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

pub(crate) fn breadcrumb(message: &str) {
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

/// True when every changed line of a Rust file's patch is a `//` comment or
/// blank. Any other file type, or an empty patch (binary, oversized), reads
/// false: only a positive reading releases the hold. A line inside a
/// multi-line string literal that starts with `//` would read as a comment;
/// no paint file holds one.
fn patch_is_comment_only(file: &str, patch: &str) -> bool {
    if !file.ends_with(".rs") || patch.is_empty() {
        return false;
    }
    let mut changed = false;
    for line in patch.lines() {
        let Some(body) = line
            .strip_prefix('+')
            .or_else(|| line.strip_prefix('-'))
            .filter(|_| !line.starts_with("+++") && !line.starts_with("---"))
        else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        if !body.starts_with("//") {
            return false;
        }
        changed = true;
    }
    changed
}

/// Whether every touched paint file changed by comment lines only. A rename
/// or an unreadable patch list reads false, so the hold stays.
fn comment_only_on<P: Probes>(probes: &P, cwd: &Path, pr: u64, touched: &[&str]) -> bool {
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/pulls/{pr}/files"),
        "--paginate".to_string(),
        "--jq".to_string(),
        ".[] | {f: .filename, s: .status, p: (.patch // \"\")} | @json".to_string(),
    ];
    let Ok((true, stdout)) = probes.run_gh(cwd, &args) else {
        return false;
    };
    let rows: Vec<Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    touched.iter().all(|path| {
        rows.iter().any(|row| {
            row["f"].as_str() == Some(path)
                && row["s"].as_str() == Some("modified")
                && patch_is_comment_only(path, row["p"].as_str().unwrap_or(""))
        })
    })
}

/// The visual-approval gate: a PR whose changed files touch a configured
/// paint path holds until an ANSWERED question page names the PR, or a
/// role-recorded decision row transcribes the user's chat or board approval
/// of this exact head. The user's look is the only clear, mechanizing the prose
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
    // Comments and code names cannot move a pixel, so a look would judge
    // nothing. The code diff alone clears the gate: no shots, no ruling, no
    // question. Every unreadable patch keeps the hold.
    if comment_only_on(probes, cwd, pr, &touched)
        || touched.iter().all(|f| {
            paint_patch(probes, cwd, pr, f).is_some_and(|p| visual_screen::names_only_patch(&p))
        })
    {
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
        || law_chat_clears_pr(probes, cwd, pr, head)
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
         decision whose text names the PR and its head sha (`fno backlog decide <node> \
         'Approved: PR {pr} at {head}' --authority role --rationale 'user in chat: \
         <the user's words>'`). An answer the user left on a board question \
         whose text names the PR and head sha records itself as law and clears \
         this gate; never transcribe a board answer as 'user in chat'. When the \
         paint edits rename identifiers only and every rendered string stays \
         byte-identical, record the ruling instead (`--rationale 'identifier-only \
         paint diff: <what renamed>; <a rendered string that proves no pixel \
         change>'`) - a look would judge nothing and no shots exist to file. Ask via \
         `fno inbox outstanding ask`, then the user answers the page or the lead \
         records the approval. Pass each shot with --image.",
        touched.join(", ")
    );
    if touched.iter().any(|f| is_public_copy(f)) {
        detail.push_str(
            "; this PR changes public copy (README or docs/images). Before the page \
             reaches the user, brief a fresh subagent as the target developer, told \
             nothing else, and have it answer in writing: what does this show, would \
             it make you install fno, and what number or claim here hurts. Put the \
             answer on the question page. A 'hurts' answer sends the change back \
             before the user sees it",
        );
    }
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
            let mux = crate::escalation::vault_dir_with_home(&root, home.as_deref(), "mux");
            let doc = std::fs::read_to_string(root.join("docs/architecture/mux-design-system.md"))
                .unwrap_or_default();
            let headings = visual_screen::rule_headings(&doc);
            let remedy = format!(
                "run `fno-agents visual critique-prep {node}`, then the `design-critic` skill on \
                 {node}-critique.json, fix every high finding, and re-read status at the new head"
            );
            match visual_screen::read_critique(&mux, node, head, &headings) {
                visual_screen::CritiqueRead::Missing => detail.push_str(&format!(
                    "; no design critique at this head, so the gate files no page: {remedy}"
                )),
                visual_screen::CritiqueRead::Unfilled(why) => detail.push_str(&format!(
                    "; the design critique is not usable ({why}), so the gate files no page: {remedy}"
                )),
                visual_screen::CritiqueRead::High(high) => detail.push_str(&format!(
                    "; the design critic left {} high finding(s), so the gate files no page: {}; fix \
                     each, then {remedy}",
                    high.len(),
                    high.join("; ")
                )),
                visual_screen::CritiqueRead::Ready(c) => {
                    let mut images: Vec<PathBuf> = shots.iter().map(|s| mux.join(s)).collect();
                    images.extend(c.views.iter().filter_map(|v| v.boxed.as_ref()).map(|b| mux.join(b)));
                    detail.push_str(&file_visual_question(
                        &root,
                        pr,
                        head,
                        node,
                        &touched.join(", "),
                        &visual_screen::critique_summary(&c),
                        &images,
                    ));
                }
            }
        }
    }
    Some(Blocker::held("visual_approval", detail))
}

/// The unified-diff patch GitHub holds for one changed file of the PR.
/// `None` when the read fails or GitHub sent no patch (binary, too large).
fn paint_patch<P: Probes>(probes: &P, cwd: &Path, pr: u64, file: &str) -> Option<String> {
    let filter = format!(".[] | select(.filename == \"{file}\") | .patch // empty");
    let args: Vec<String> = [
        "api",
        &format!("repos/{{owner}}/{{repo}}/pulls/{pr}/files"),
        "--paginate",
        "--jq",
        &filter,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let (ok, out) = probes.run_gh(cwd, &args).ok()?;
    (ok && !out.trim().is_empty()).then_some(out)
}

/// The public front page a reader meets before any install: the README and
/// its images. Landing copy lives in the docs tree and takes the same path.
fn is_public_copy(path: &str) -> bool {
    path == "README.md" || path.starts_with("docs/images/")
}

/// How many earlier commits of the PR the carry walks, newest first.
const CARRY_WALK: usize = 30;

/// The user's look sticks to the PR until merge: a later commit moves the
/// head, and the approval holds. The newest earlier commit of this PR that
/// the user approved is the reference, kept on the record for audit. The
/// hold returns when the user declined a later commit, or when the head
/// paints a file the approved patch never showed. Every unreadable read
/// holds.
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
    let mut texts: Vec<String> = approving_pages(root, pr)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    texts.extend(
        node.map(|n| role_approval_texts(probes, cwd, pr, n))
            .unwrap_or_default(),
    );
    texts.extend(law_approval_texts(probes, cwd, pr));
    let Some(approved) = commits
        .iter()
        .rev()
        .filter(|sha| sha.as_str() != head)
        .take(CARRY_WALK)
        .find(|sha| texts.iter().any(|text| text_names_head(text, sha)))
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
    let newer: Vec<&str> = commits
        .iter()
        .skip_while(|sha| sha.as_str() != approved.as_str())
        .skip(1)
        .map(String::as_str)
        .collect();
    if declined_after(&declining_pages(root, pr), &newer) {
        return false;
    }
    match (
        paint_files(probes, cwd, &base, approved, paint_paths),
        paint_files(probes, cwd, &base, head, paint_paths),
    ) {
        (Some(was), Some(now)) => adds_no_paint_surface(&was, &now),
        _ => false,
    }
}

/// An approval sticks to the PR: every paint file the head changes was
/// already in the approved patch, so the user saw that surface. A paint file
/// the approval never showed is a new look.
fn adds_no_paint_surface(was: &BTreeSet<String>, now: &BTreeSet<String>) -> bool {
    !now.is_empty() && now.is_subset(was)
}

/// Did the user decline the paint at a commit pushed after the approval?
/// `newer` holds the commits after the approved one. A decline page names
/// the head it judged.
fn declined_after(declines: &[String], newer: &[&str]) -> bool {
    declines
        .iter()
        .any(|text| newer.iter().any(|sha| text_names_head(text, sha)))
}

/// The paint files the PR's three-dot compare against the base changes at
/// one commit.
fn paint_files<P: Probes>(
    probes: &P,
    cwd: &Path,
    base: &str,
    sha: &str,
    paint_paths: &[String],
) -> Option<BTreeSet<String>> {
    let lines = gh_lines(
        probes,
        cwd,
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/compare/{base}...{sha}"),
            "--jq",
            ".files[].filename",
        ],
    )?;
    Some(
        lines
            .into_iter()
            .filter(|name| path_matches_paint(name, paint_paths))
            .collect(),
    )
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
pub(crate) fn mux_shots(root: &Path, home: Option<&Path>, node: &str) -> Vec<String> {
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
/// reversible question reach the user at all. The class word stands bare:
/// the ask door's asker-decides refusal reads the value as exactly one
/// class word, and prose after it reads as no class, refusing the very
/// page this gate files. The shots ride `--image`, so
/// the page shows them under its Screens heading.
fn visual_question_markdown(
    pr: u64,
    head: &str,
    short: &str,
    node: &str,
    files: &str,
    summary: &str,
) -> String {
    let changed = if summary.is_empty() {
        String::new()
    } else {
        format!(
            "## What the critic checked and left\n\
             The design critic read the main and PR shots against the mux design system. \
             A red box frames each changed region under Screens.\n\
             {summary}\n\n"
        )
    };
    format!(
        "---\n\
         recommend: 1\n\
         why_user: taste\n\
         ---\n\n\
         May PR {pr} merge as painted? ({node} at {short})\n\n\
         {changed}\
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
    node: &str,
    files: &str,
    summary: &str,
    shots: &[PathBuf],
) -> String {
    let short = head.chars().take(8).collect::<String>();
    if visual_page_filed(root, pr, head) {
        return format!("; the question page for PR {pr} at {short} is already filed");
    }
    let path = std::env::temp_dir().join(format!(
        "fno-visual-ask-{pr}-{short}-{}.md",
        std::process::id()
    ));
    let markdown = visual_question_markdown(pr, head, &short, node, files, summary);
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
    !head.is_empty()
        && approving_pages(root, pr)
            .iter()
            .any(|(_, text)| text.contains(head))
}

/// Every ANSWERED question page that picks the approve option and names the
/// PR, as `(question id, page text)`, at any head.
fn approving_pages(root: &Path, pr: u64) -> Vec<(String, String)> {
    let dir = crate::escalation::questions_dir(root);
    // Answered pages are archived into done/ after the fact; an approval
    // must not lapse because its page moved there.
    [dir.clone(), dir.join("done")]
        .iter()
        .flat_map(|d| crate::lead_answers::read_question_pages(d).unwrap_or_default())
        .filter_map(|(_stem, text)| {
            let (front, _) = crate::attention_file::parse_page(&text)?;
            (front.status == "answered"
                && front.answer.as_deref().is_some_and(answer_approves)
                && page_names_pr(&text, pr))
            .then_some((front.question_id, text))
        })
        .collect()
}

/// Every ANSWERED question page that names the PR and does not pick the
/// approve option (hold for changes): the page text.
fn declining_pages(root: &Path, pr: u64) -> Vec<String> {
    let dir = crate::escalation::questions_dir(root);
    [dir.clone(), dir.join("done")]
        .iter()
        .flat_map(|d| crate::lead_answers::read_question_pages(d).unwrap_or_default())
        .filter_map(|(_stem, text)| {
            let (front, _) = crate::attention_file::parse_page(&text)?;
            (front.status == "answered"
                && front.answer.as_deref().is_some_and(|a| !answer_approves(a))
                && page_names_pr(&text, pr))
            .then_some(text)
        })
        .collect()
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

/// The rationale attests the paint diff is identifier-only: the edits
/// rename or rebind code names and every rendered string stays
/// byte-identical, so no pixel moves and a look would judge nothing. The
/// shape matters where the ask door refuses a reversible question and no
/// shots exist to file a page with, which left a held paint PR no lane at
/// all. Both spellings a lead writes read; the ruling stays role-gated and
/// head-scoped like the chat attestation, so only the lead whose territory
/// covers the node can mint it and a push voids it.
fn row_attests_identifier_only(row: &Value) -> bool {
    row.get("rationale")
        .and_then(Value::as_str)
        .is_some_and(|r| {
            let r = r.to_lowercase();
            r.contains("identifiers only") || r.contains("identifier-only")
        })
}

/// The rationale cites one of these question ids as a whole token. The
/// user answered that page on the board, so the board is the channel and
/// the lead need not write a chat attestation that would be false.
fn row_cites_any(row: &Value, ids: &[String]) -> bool {
    let Some(r) = row.get("rationale").and_then(Value::as_str) else {
        return false;
    };
    let tokens: Vec<&str> = r
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .collect();
    ids.iter().any(|id| tokens.contains(&id.as_str()))
}

/// True when a live role decision row transcribes the user's approval of
/// this PR at this head: authority `role`, the PR's bound node as subject
/// (the decide door lets only the lead whose territory covers that node mint
/// it), a rationale that attests the user's chat words, cites an answered
/// question page that approves this PR, or attests the identifier-only paint
/// shape, and text naming both the PR and the
/// exact head sha the lead re-pinned. Head-scoped like the operator head grant: a
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
            .any(|text| text_names_head(text, head))
}

/// One `fno backlog decisions ... --json` read; None when the read fails.
fn decisions_payload<P: Probes>(probes: &P, cwd: &Path, args: &[&str]) -> Option<Value> {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let Some((Some(0), out, _)) = probes.fno_shell(cwd, &args).ok() else {
        return None;
    };
    serde_json::from_slice::<Value>(&out).ok()
}

/// The text names the head: the full sha, or a hex token of at least ten
/// characters that begins it. A question card abbreviates the head it asks
/// about, and the user's answer on the board inherits that spelling.
fn text_names_head(text: &str, head: &str) -> bool {
    !head.is_empty()
        && (text.contains(head)
            || text
                .split(|c: char| !c.is_ascii_hexdigit())
                .any(|t| t.len() >= 10 && head.starts_with(t)))
}

/// A board answer recorded as a decision: the user answered a question the
/// board put to them, so the row carries the question text that names the PR
/// and head, and the answer text that picks the approve option. The page
/// stays open when its close is lost; the decision still holds the answer.
fn board_approval_text(row: &Value, pr: u64) -> Option<String> {
    let trace = row.get("trace")?;
    let from_board = trace.get("comms").and_then(Value::as_str) == Some("question")
        && trace.get("recipient_kind").and_then(Value::as_str) == Some("user");
    let question = row.get("question").and_then(Value::as_str)?;
    let answer = row.get("decision").and_then(Value::as_str)?;
    // The asking team may close its own question with a role, team, agent or
    // beastmode answer. Only the user's own answer (no authority stamp, or
    // an operator or quoted chat stamp) is a look.
    let by_user = match row.get("authority_source").and_then(Value::as_str) {
        None | Some("operator") => true,
        Some("chat_attested") => row
            .get("quote")
            .and_then(Value::as_str)
            .is_some_and(|q| !q.trim().is_empty()),
        Some(_) => false,
    };
    (from_board
        && by_user
        && row.get("question_id").is_some()
        && answer_approves(answer)
        && page_names_pr(question, pr))
    .then(|| question.to_string())
}

/// The text of every live role row on this node that approves this PR, at
/// any head: one decisions read, so a caller can test many heads with it.
/// A board answer on the node counts too (see `board_approval_text`).
fn role_approval_texts<P: Probes>(probes: &P, cwd: &Path, pr: u64, node: &str) -> Vec<String> {
    // Subject-scoped and uncapped: a capped newest-first read of the whole
    // lane loses an approval once enough other rows land after it.
    let Some(payload) = decisions_payload(
        probes,
        cwd,
        &[
            "backlog",
            "decisions",
            node,
            "--lane",
            "unattributed",
            "--state",
            "all",
            "--limit",
            "0",
            "--json",
        ],
    ) else {
        return Vec::new();
    };
    let approved: Vec<String> = approving_pages(&vault_root(cwd), pr)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| !id.is_empty())
        .collect();
    let rows = payload
        .get("decisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let mut texts = Vec::new();
    for row in rows {
        if row.get("subject").and_then(Value::as_str) != Some(node) {
            continue;
        }
        if let Some(text) = board_approval_text(row, pr) {
            texts.push(text);
            continue;
        }
        let live = matches!(
            row.get("lifecycle").and_then(Value::as_str),
            Some("live") | Some("unscoped")
        );
        if row.get("authority_source").and_then(Value::as_str) == Some("role")
            && live
            && (row_is_chat_attested(row)
                || row_cites_any(row, &approved)
                || row_attests_identifier_only(row))
        {
            let text = row_text(row);
            if page_names_pr(&text, pr) {
                texts.push(text);
            }
        }
    }
    texts
}

/// The text of every live law row that approves this PR, at any head: the
/// chat-attested row the worker the user spoke to records (`fno inbox law
/// set`), or the operator law the question board records for the user's own
/// board answer. The lead who owns the gate cannot re-record an approval it
/// never witnessed. A chat row must carry the user's words (a `user in
/// chat` rationale or a stored `quote`), name the PR, and open with an
/// approve word. A board row reads through `board_approval_text`, which
/// identifies it by trace (comms question, recipient user), authority and
/// question id, not by `attested_by`; its question text names the PR and
/// its answer must approve. The head test stays with the caller.
fn law_approval_texts<P: Probes>(probes: &P, cwd: &Path, pr: u64) -> Vec<String> {
    let Some(payload) = decisions_payload(
        probes,
        cwd,
        &[
            "backlog",
            "decisions",
            "--lane",
            "law",
            "--state",
            "live",
            "--limit",
            "0",
            "--json",
        ],
    ) else {
        return Vec::new();
    };
    let mut texts = Vec::new();
    for row in payload
        .get("decisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // The board's own law row: the question text names the PR and the
        // head it asked about, and the answer picks the approve option.
        if let Some(text) = board_approval_text(row, pr) {
            texts.push(text);
            continue;
        }
        let chat_cleared = row.get("authority_source").and_then(Value::as_str)
            == Some("chat_attested")
            && row.get("lifecycle").and_then(Value::as_str) == Some("live")
            && (row_is_chat_attested(row)
                || row
                    .get("quote")
                    .and_then(Value::as_str)
                    .is_some_and(|q| !q.trim().is_empty()))
            && row
                .get("decision")
                .and_then(Value::as_str)
                .is_some_and(answer_approves);
        if chat_cleared {
            let text = row_text(row);
            if page_names_pr(&text, pr) {
                texts.push(text);
            }
        }
    }
    texts
}

/// True when a live law row that approves this PR (chat-attested, or the
/// question board's own operator-law row for the user's board answer) names
/// the head sha, full or as a ten-character prefix. Head-scoped like the
/// role row: a push voids it.
pub(crate) fn law_chat_clears_pr<P: Probes>(probes: &P, cwd: &Path, pr: u64, head: &str) -> bool {
    !head.is_empty()
        && law_approval_texts(probes, cwd, pr)
            .iter()
            .any(|text| text_names_head(text, head))
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
    fn readme_and_doc_images_are_public_copy_and_other_docs_are_not() {
        assert!(is_public_copy("README.md"));
        assert!(is_public_copy("docs/images/target-run.gif"));
        assert!(!is_public_copy("docs/architecture/pr-status-verdict.md"));
        assert!(!is_public_copy("crates/fno/README.md"));
        assert!(path_matches_paint(
            "docs/images/target-run.gif",
            &["docs/images/**".to_string()]
        ));
    }

    #[test]
    fn a_paint_diff_of_comments_only_skips_the_look() {
        let comments = "@@ -1,2 +1,2 @@\n ctx\n-// a busy box\n+// a busy machine\n+\n";
        assert!(patch_is_comment_only("a.rs", comments));
        let code = "@@ -1 +1 @@\n-let a = \"box\"; // c\n+let a = \"machine\"; // c\n";
        assert!(!patch_is_comment_only("a.rs", code));
        let mixed = "@@ -1 +1 @@\n-// old\n+// new\n+draw();\n";
        assert!(!patch_is_comment_only("a.rs", mixed));
        // Only Rust files read as comments; an html `//` is script code.
        assert!(!patch_is_comment_only("page.html", comments));
        // An empty patch (binary, oversized) never releases the hold.
        assert!(!patch_is_comment_only("a.rs", ""));
    }

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
                contract_node_for_pr(&entries, pr, "o/r").as_deref(),
                Some("x-3333"),
                "pr {pr} must match"
            );
        }
        assert_eq!(contract_node_for_pr(&entries, 8, "o/r"), None);
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

    #[test]
    fn an_approval_holds_for_edits_to_files_it_showed_and_not_for_a_new_surface() {
        let map = |names: &[&str]| {
            names
                .iter()
                .map(|n| (*n).to_string())
                .collect::<BTreeSet<_>>()
        };
        let was = map(&["a.rs", "b.rs"]);
        assert!(adds_no_paint_surface(&was, &map(&["a.rs"])));
        assert!(adds_no_paint_surface(&was, &map(&["a.rs", "b.rs"])));
        assert!(!adds_no_paint_surface(&was, &map(&["a.rs", "c.rs"])));
        assert!(!adds_no_paint_surface(&was, &map(&[])));
    }

    #[test]
    fn a_decline_after_the_approved_commit_voids_the_carry() {
        let declines = vec!["May PR 9 merge? answer 2 at c3".to_string()];
        assert!(declined_after(&declines, &["c3", "c4"]));
        let spelled = vec!["May PR 9 merge? answer 2 at 0123456789".to_string()];
        assert!(declined_after(
            &spelled,
            &["0123456789abcdef0123456789abcdef01234567"]
        ));
        assert!(!declined_after(&declines, &["c4"]));
        assert!(!declined_after(&[], &["c3"]));
    }

    #[test]
    fn an_identifier_only_ruling_reads_as_attested_and_other_rationales_do_not() {
        let attested = |r: &str| row_attests_identifier_only(&json!({ "rationale": r }));
        assert!(attested(
            "Visual approval is granted by the lead. The paint-file edits rename local \
             identifiers only (the action name became the command name). Every rendered \
             string stays byte-identical, e.g. tab_prune.rs still prints 'would retire' \
             and 'pruned'. No pixel changes."
        ));
        assert!(attested(
            "identifier-only paint diff: fn run renamed to fn cmd; footer still prints \
             'would retire'"
        ));
        // A look lane and a board citation are their own attestations, not
        // this one; a bare approval with no identifier claim never reads as
        // identifier-only.
        assert!(!attested("user in chat: approved the splash"));
        assert!(!attested("user answered q-1 on the board"));
        assert!(!attested("Approved without a look"));
        assert!(!attested(""));
        assert!(!row_attests_identifier_only(&json!({})));
    }

    #[test]
    fn the_visual_template_names_the_taste_class_bare() {
        // The ask door's asker-decides refusal reads why_user as exactly one
        // class word; prose after the word reads as no class and would
        // refuse the page this gate files.
        let page = visual_question_markdown(
            3036,
            "05ea5bcccc12",
            "05ea5bcc",
            "x-aaaa",
            "client/tab.rs",
            "",
        );
        let why = page
            .lines()
            .find(|l| l.starts_with("why_user:"))
            .expect("why_user line");
        assert_eq!(why, "why_user: taste");
    }

    /// The specimen behind the scoping rule: the ledger is global and fnh
    /// PR 70 exists, so a fno-web checkout asking about its own PR 70 saw
    /// fnh's plan answer and the fidelity gate refused the wrong PR. A row
    /// whose pr_url names another repo never answers; a slug-matching row or
    /// a row with no pr_url still does.
    #[test]
    fn a_foreign_repos_pr_url_never_answers_a_shared_pr_number() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Saved-env guard: other tests read these pins, so restore, not remove.
        // Claiming hermetic manufactures the fence's claim, so every root the
        // fence judges must be pinned under std::env::temp_dir() here: the CI
        // cargo job exports FNO_AGENTS_HOME=$RUNNER_TEMP/fno-agents for the
        // whole job, and an unpinned claim made the fence refuse that ambient
        // value (the cli-ci red this test shipped).
        struct Pins {
            hermetic: Option<std::ffi::OsString>,
            state: Option<std::ffi::OsString>,
            agents_home: Option<std::ffi::OsString>,
            spaces: Option<std::ffi::OsString>,
            fno_home: Option<std::ffi::OsString>,
        }
        impl Drop for Pins {
            fn drop(&mut self) {
                match &self.hermetic {
                    Some(v) => std::env::set_var("FNO_TEST_HERMETIC", v),
                    None => std::env::remove_var("FNO_TEST_HERMETIC"),
                }
                match &self.state {
                    Some(v) => std::env::set_var("FNO_STATE_DIR", v),
                    None => std::env::remove_var("FNO_STATE_DIR"),
                }
                match &self.agents_home {
                    Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
                    None => std::env::remove_var("FNO_AGENTS_HOME"),
                }
                match &self.spaces {
                    Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                    None => std::env::remove_var("FNO_SPACES_DIR"),
                }
                match &self.fno_home {
                    Some(v) => std::env::set_var("FNO_HOME", v),
                    None => std::env::remove_var("FNO_HOME"),
                }
            }
        }
        let _pins = Pins {
            hermetic: std::env::var_os("FNO_TEST_HERMETIC"),
            state: std::env::var_os("FNO_STATE_DIR"),
            agents_home: std::env::var_os("FNO_AGENTS_HOME"),
            spaces: std::env::var_os("FNO_SPACES_DIR"),
            fno_home: std::env::var_os("FNO_HOME"),
        };
        std::env::set_var("FNO_TEST_HERMETIC", "1");
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_STATE_DIR", state.path());
        std::env::set_var("FNO_AGENTS_HOME", state.path().join("agents-home"));
        std::env::set_var("FNO_SPACES_DIR", state.path().join("spaces"));
        std::env::set_var("FNO_HOME", state.path().join("fno-home"));

        // The checkout: fno-web origin, so the slug is fnodotdev/fno-web.
        let repo = tempfile::tempdir().unwrap();
        for args in [
            ["init", "-q"].as_slice(),
            [
                "config",
                "remote.origin.url",
                "git@github.com:fnodotdev/fno-web.git",
            ]
            .as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?} failed");
        }

        let ledger_path = crate::paths_cli::ledger_json(repo.path());
        std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
        let write_ledger = |entries: serde_json::Value| {
            std::fs::write(
                &ledger_path,
                serde_json::to_string(&json!({ "entries": entries })).unwrap(),
            )
            .unwrap();
        };
        let fnh_row = json!({
            "pr_number": 70,
            "pr_url": "https://github.com/fnodotdev/fnh/pull/70",
            "plan_path": "/fnh/plan.md",
        });

        // The specimen: only the other repo's row exists. It names another
        // repo, so it never answers and the gate stays silent.
        write_ledger(json!([fnh_row]));
        assert_eq!(ledger_plan_path(repo.path(), 70), None);

        // An unresolvable origin cannot prove any pr_url is ours, so a row
        // that carries one stays silent there too.
        let bare = tempfile::tempdir().unwrap();
        write_ledger(json!([fnh_row]));
        assert_eq!(ledger_plan_path(bare.path(), 70), None);

        // A row with no pr_url may answer: unmarked provenance keeps the old
        // first-match fail-open shape.
        write_ledger(json!([
            fnh_row,
            json!({"pr_number": 70, "plan_path": "/web/plan.md"})
        ]));
        assert_eq!(
            ledger_plan_path(repo.path(), 70).as_deref(),
            Some("/web/plan.md")
        );

        // The slug match outranks the rest, whatever order the rows hold.
        write_ledger(json!([
            fnh_row,
            json!({
                "pr_number": 70,
                "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
                "plan_path": "/web/chosen.md",
            }),
        ]));
        assert_eq!(
            ledger_plan_path(repo.path(), 70).as_deref(),
            Some("/web/chosen.md")
        );

        // A prefix-sibling repo's url contains this slug as a substring but
        // names a different path segment; it never reads as a match.
        write_ledger(json!([json!({
            "pr_number": 70,
            "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
            "plan_path": "/web/plan.md",
        })]));
        let sibling = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(sibling.path())
            .status()
            .expect("git runs");
        assert!(status.success(), "git init failed");
        let status = std::process::Command::new("git")
            .args([
                "config",
                "remote.origin.url",
                "git@github.com:fnodotdev/fno.git",
            ])
            .current_dir(sibling.path())
            .status()
            .expect("git runs");
        assert!(status.success(), "git config failed");
        let sibling_ledger = crate::paths_cli::ledger_json(sibling.path());
        std::fs::create_dir_all(sibling_ledger.parent().unwrap()).unwrap();
        std::fs::write(
            &sibling_ledger,
            serde_json::to_string(&json!({ "entries": [json!({
                "pr_number": 70,
                "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
                "plan_path": "/web/plan.md",
            })] }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(ledger_plan_path(sibling.path(), 70), None);
    }

    /// The graph twin of the ledger scoping rule: the store is global, so a
    /// contract node whose pr_url names another repo never answers a shared
    /// number. A url-less node keeps the fail-open answer.
    #[test]
    fn a_contract_node_of_another_repo_never_answers_a_shared_pr_number() {
        let ours = json!({
            "id": "x-bbbb",
            "dep": "contract",
            "pr_number": 70,
            "pr_url": "https://github.com/fnodotdev/fno-web/pull/70",
        });
        let foreign = json!({
            "id": "x-aaaa",
            "dep": "contract",
            "pr_number": 70,
            "pr_url": "https://github.com/fnodotdev/fnh/pull/70",
        });
        let slug = "fnodotdev/fno-web";

        // Only the foreign node: silent.
        assert_eq!(
            contract_node_for_pr(&[foreign.clone()], 70, slug),
            None,
            "a foreign repo's contract node never answers"
        );

        // Ours answers, whatever order the rows hold.
        assert_eq!(
            contract_node_for_pr(&[foreign.clone(), ours.clone()], 70, slug).as_deref(),
            Some("x-bbbb")
        );
        assert_eq!(
            contract_node_for_pr(&[ours, foreign], 70, slug).as_deref(),
            Some("x-bbbb")
        );

        // A url-less node keeps the fail-open answer.
        let bare = json!({
            "id": "x-cccc",
            "dep": "contract",
            "pr_number": 70,
        });
        assert_eq!(
            contract_node_for_pr(&[bare], 70, slug).as_deref(),
            Some("x-cccc")
        );
    }
}
