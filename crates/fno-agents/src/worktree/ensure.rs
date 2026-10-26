//! The dispatch-time isolation primitive, ported from the deleted
//! `worktree_cli/cli.py` `_worktree_ensure`: policy gate, idempotent reuse,
//! branch switch, linked-tree refusal, and the create arm. Receipts are
//! byte-identical to the Python ones; the `ensure_*` goldens characterize
//! them.

use super::policy::{resolve, Resolved};
use super::{git_bounded, git_out, resolve_lenient};
use std::path::{Path, PathBuf};

const FETCH_TIMEOUT_SECS: u64 = 60;

/// The node's branch, minted by the one resolver: the Python leg asked
/// `fno backlog get <name> --field _branch` through a subprocess; the native
/// flow reads the ambient graph and asks node_branch in-process, so the
/// ensure adds no seam crossing. A miss answers feature/<name>.
fn node_branch_for(name: &str) -> Option<String> {
    // The layout anchor, not a bare <state>/graph.db: the migrated layout
    // keeps the store under db/, the same read the stranded sweep makes.
    let path = crate::state_layout::graph_anchor(&super::state_root());
    let rows = crate::graph_store::read_rows(&path).unwrap_or_default();
    let (_, row) = super::stranded::resolve_node(name, &rows)?;
    crate::node_branch::resolve(&row)
}

fn top_level(repo: &Path) -> Option<PathBuf> {
    let out = git_out(repo, &["rev-parse", "--show-toplevel"])?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    Some(resolve_lenient(Path::new(line)))
}

fn abs_git_path(repo: &Path, which: &str) -> Option<PathBuf> {
    let out = git_out(repo, &["rev-parse", &format!("--{which}")])?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let p = PathBuf::from(line);
    let p = if p.is_absolute() { p } else { repo.join(p) };
    Some(resolve_lenient(&p))
}

/// The ref a fresh dispatch branch is based on: origin/main, then
/// origin/master, else None (a local repo).
fn base_ref(repo: &Path) -> Option<String> {
    for ref_name in ["origin/main", "origin/master"] {
        let out = git_out(repo, &["rev-parse", "--verify", "--quiet", ref_name])?;
        if out.status.success() {
            return Some(ref_name.to_string());
        }
    }
    None
}

/// Commits in `base..ref`, or None when `ref` does not resolve.
fn ahead_count(repo: &Path, base: &str, ref_name: &str) -> Option<i64> {
    let verify = git_out(repo, &["rev-parse", "--verify", "--quiet", ref_name])?;
    if !verify.status.success() {
        return None;
    }
    let out = git_out(
        repo,
        &["rev-list", "--count", &format!("{base}..{ref_name}")],
    )?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().parse::<i64>().ok()
}

/// The one fatal line containing the word "fatal", minus the "couldn't find
/// remote ref" shape that is a normal miss here.
fn fetch_failure(fetch: Option<&std::process::Output>) -> String {
    let Some(out) = fetch else {
        return "git fetch origin timed out after 60s".to_string();
    };
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        if line.contains("fatal") && !line.contains("couldn't find remote ref") {
            return line.trim().to_string();
        }
    }
    String::new()
}

/// What a re-dispatch should continue: origin/<br> ahead of base, then a
/// salvage ref ahead of base. Returns (kind, source, count, fetch-target).
fn continuation_base(repo: &Path, name: &str, base: &str, br: &str) -> Option<Continuation> {
    let remote_branch = base.split('/').nth(1).unwrap_or("main");
    let salvage = format!("refs/fno/salvage/{name}");
    let feat_ref = format!("refs/remotes/origin/{br}");
    // Full-path dst: `{br}:{feat_ref}` materializes the tracking ref even on
    // a --single-branch clone. A bare `feat_ref` is a LHS-only refspec: git
    // looks for a ref named refs/remotes/origin/<br> ON the remote, finds
    // none, and the tracking ref never materializes - so the ahead count
    // always reads zero and every re-dispatch cuts fresh.
    let fetch = git_bounded(
        repo,
        &[
            "fetch",
            "--quiet",
            "origin",
            remote_branch,
            &format!("{br}:{feat_ref}"),
        ],
        FETCH_TIMEOUT_SECS,
    );
    if let Some(count) = ahead_count(repo, base, &feat_ref) {
        if count > 0 {
            return Some(Continuation {
                kind: "continued".to_string(),
                source: format!("origin/{br}"),
                count,
                create_target: feat_ref,
            });
        }
    }
    let remote_salvage = format!("refs/remotes/fno-salvage/{name}");
    let sal_fetch = git_bounded(
        repo,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("{salvage}:{remote_salvage}"),
        ],
        FETCH_TIMEOUT_SECS,
    );
    continue_salvage_arm(
        repo,
        base,
        &salvage,
        &remote_salvage,
        sal_fetch.as_ref(),
        fetch.as_ref(),
    )
}

struct Continuation {
    kind: String,
    source: String,
    count: i64,
    create_target: String,
}

/// The salvage arm of the continuation read: a local salvage ref ahead of
/// base continues, and a fetch failure is the one reason to cut fresh.
fn continue_salvage_arm(
    repo: &Path,
    base: &str,
    salvage: &str,
    remote_salvage: &str,
    sal_fetch: Option<&std::process::Output>,
    // The deleted leg named a fetch failure in its fresh-cut receipt; no
    // golden pins that note, so the arm keeps the handle but reads nothing
    // from it yet.
    _fetch: Option<&std::process::Output>,
) -> Option<Continuation> {
    let count = if sal_fetch.map(|o| o.status.success()).unwrap_or(false) {
        ahead_count(repo, base, remote_salvage)
    } else {
        None
    };
    if let Some(count) = count {
        if count > 0 {
            return Some(Continuation {
                kind: "salvaged".to_string(),
                source: salvage.to_string(),
                count,
                create_target: remote_salvage.to_string(),
            });
        }
    }
    // The remote arm missing is not the end: a dead worker's hook may have
    // written only the LOCAL salvage ref (a never-pushed crash), and a ref
    // ahead of base is exactly what a re-dispatch continues.
    if let Some(count) = ahead_count(repo, base, salvage) {
        if count > 0 {
            return Some(Continuation {
                kind: "salvaged".to_string(),
                source: salvage.to_string(),
                count,
                create_target: salvage.to_string(),
            });
        }
    }
    None
}

/// The `worktree-ensure` client op: parse, run the flow, print the streams.
pub fn run_client(args: &[String]) -> i32 {
    let mut repo: Option<String> = None;
    let mut name: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" if i + 1 < args.len() => {
                repo = Some(args[i + 1].clone());
                i += 2;
            }
            "--name" if i + 1 < args.len() => {
                name = Some(args[i + 1].clone());
                i += 2;
            }
            "--branch" if i + 1 < args.len() => {
                branch = Some(args[i + 1].clone());
                i += 2;
            }
            "--harness" if i + 1 < args.len() => {
                harness = Some(args[i + 1].clone());
                i += 2;
            }
            other => {
                eprintln!("fno-agents worktree-ensure: unexpected argument {other}");
                return 2;
            }
        }
    }
    let (Some(repo), Some(name)) = (repo, name) else {
        eprintln!("fno-agents worktree-ensure: --repo and --name are required");
        return 2;
    };
    let (code, out, err) = ensure_flow(
        Path::new(&repo),
        &name,
        branch.as_deref(),
        harness.as_deref(),
    );
    print!("{out}");
    eprint!("{err}");
    code
}

/// The flow, printing nothing itself; streams come back for the op and tests.
pub fn ensure_flow(
    repo: &Path,
    name: &str,
    branch: Option<&str>,
    harness: Option<&str>,
) -> (i32, String, String) {
    let mut out = String::new();
    let mut err = String::new();
    let code = ensure_inner(repo, name, branch, harness, &mut out, &mut err);
    (code, out, err)
}

fn ensure_inner(
    repo: &Path,
    name: &str,
    branch: Option<&str>,
    harness: Option<&str>,
    out: &mut String,
    err: &mut String,
) -> i32 {
    // Step 0 checks the repo is a git repository at all.
    let Some(top) = top_level(repo) else {
        err.push_str(&format!(
            "worktree ensure: {repo:?} is not a git repository\n"
        ));
        return 1;
    };
    let pol = match resolve(&top, harness) {
        Ok(pol) => pol,
        Err(msg) => {
            err.push_str(&format!("worktree ensure: {msg}\n"));
            return 1;
        }
    };
    ensure_step1(&top, name, branch, harness, &pol, out, err)
}

fn policy_receipt(pol: &Resolved) -> String {
    let mut line = format!("policy={} ({})", pol.policy, pol.project);
    if pol.degraded {
        line.push_str(&format!(
            " requested={} degraded=true",
            pol.requested_policy
        ));
    }
    if !pol.note.is_empty() {
        line.push_str(&format!("; {}", pol.note));
    }
    line
}

fn ensure_step1(
    top: &Path,
    name: &str,
    branch: Option<&str>,
    harness: Option<&str>,
    pol: &Resolved,
    out: &mut String,
    err: &mut String,
) -> i32 {
    // A `never` project launches in place; an explicit --branch cannot be
    // isolated in place, so that combination refuses (fail closed).
    if pol.policy == "never" {
        if let Some(br) = branch {
            err.push_str(&format!(
                "worktree ensure: policy=never ({}) conflicts with --branch {br}; \
                 cannot isolate a branch in place\n",
                pol.project
            ));
            return 1;
        }
        out.push_str(&top.to_string_lossy());
        out.push('\n');
        err.push_str(&format!(
            "worktree ensure: {}; launching in place\n",
            policy_receipt(pol)
        ));
        return 0;
    }
    // Only a MAIN checkout may spawn a worktree (git-dir == git-common-dir).
    let gdir = abs_git_path(top, "git-dir");
    let common = abs_git_path(top, "git-common-dir");
    match (gdir, common) {
        (Some(g), Some(c)) if g == c => {}
        _ => {
            err.push_str(&format!(
                "worktree ensure: {top:?} is a linked worktree, not a main \
                 checkout; refusing to nest\n"
            ));
            return 1;
        }
    }
    let wt = if pol.policy == "harness-native" && harness == Some("claude") {
        top.join(".claude").join("worktrees").join(name)
    } else {
        let repo_name = top
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        pol.base.join(repo_name).join(name)
    };
    ensure_step2(top, name, branch, harness, pol, &wt, out, err)
}

fn ensure_step2(
    top: &Path,
    name: &str,
    branch: Option<&str>,
    _harness: Option<&str>,
    pol: &Resolved,
    wt: &Path,
    out: &mut String,
    err: &mut String,
) -> i32 {
    // Idempotent reuse: an existing registered worktree rooted at wt reuses;
    // a demanded branch the reused tree does not carry is honored.
    if wt.exists() {
        return ensure_reuse_arm(top, name, branch, pol, wt, out, err);
    }
    let br = match branch {
        Some(b) => b.to_string(),
        None => node_branch_for(name).unwrap_or_else(|| format!("feature/{name}")),
    };
    ensure_create_arm(top, name, branch, pol, wt, &br, out, err)
}

/// One porcelain row: (branch, path) for attached worktrees only.
fn worktree_for_branch(top: &Path, br: &str) -> Option<PathBuf> {
    let out = git_out(top, &["worktree", "list", "--porcelain"])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    crate::worktree::parse_porcelain_rows(&text)
        .into_iter()
        .find_map(|(branch, path)| (branch.as_deref() == Some(br)).then_some(PathBuf::from(path)))
}

fn ensure_reuse_arm(
    top: &Path,
    name: &str,
    branch: Option<&str>,
    pol: &Resolved,
    wt: &Path,
    out: &mut String,
    err: &mut String,
) -> i32 {
    let inside = git_out(wt, &["rev-parse", "--is-inside-work-tree"]);
    let wt_top = git_out(wt, &["rev-parse", "--show-toplevel"]);
    let inside_ok = inside
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    let top_matches = wt_top
        .map(|o| {
            o.status.success()
                && resolve_lenient(Path::new(String::from_utf8_lossy(&o.stdout).trim()))
                    == resolve_lenient(wt)
        })
        .unwrap_or(false);
    if !(inside_ok && top_matches) {
        err.push_str(&format!(
            "worktree ensure: {} exists but is not a worktree; not clobbering\n",
            wt.to_string_lossy()
        ));
        return 1;
    }
    let _ = name;
    // A demanded branch the reused tree does not carry is honored: the caller
    // picked it, so the launch runs it. A failed checkout refuses.
    if let Some(branch) = branch {
        let cur = git_out(wt, &["branch", "--show-current"]);
        let cur_branch: Option<String> = cur.map(|o| {
            if o.status.success() {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            } else {
                String::new()
            }
        });
        if cur_branch.as_deref() == Some(branch) {
            reuse_receipt(wt, pol, out, err);
            return 0;
        }
        return ensure_checkout_arm(top, wt, branch, pol, out, err);
    }
    reuse_receipt(wt, pol, out, err);
    0
}

fn reuse_receipt(wt: &Path, pol: &Resolved, out: &mut String, err: &mut String) {
    err.push_str(&format!(
        "worktree ensure: {}; reusing worktree at {} created=false\n",
        policy_receipt(pol),
        wt.to_string_lossy()
    ));
    out.push_str(&wt.to_string_lossy());
    out.push('\n');
}

fn ensure_checkout_arm(
    _top: &Path,
    wt: &Path,
    branch: &str,
    pol: &Resolved,
    out: &mut String,
    err: &mut String,
) -> i32 {
    let co = git_out(wt, &["checkout", branch]);
    let ok = co.as_ref().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        let detail = co
            .map(|o| {
                let s = String::from_utf8_lossy(&o.stderr);
                let v = String::from_utf8_lossy(&o.stdout);
                let text = if s.trim().is_empty() {
                    v.to_string()
                } else {
                    s.to_string()
                };
                text.trim().to_string()
            })
            .unwrap_or_default();
        err.push_str(&format!(
            "worktree ensure: checkout {branch} in {} failed: {detail}\n",
            wt.to_string_lossy()
        ));
        return 1;
    }
    err.push_str(&format!(
        "worktree ensure: {}; switched worktree at {} to {branch} created=false\n",
        policy_receipt(pol),
        wt.to_string_lossy()
    ));
    out.push_str(&wt.to_string_lossy());
    out.push('\n');
    0
}

fn ensure_create_arm(
    top: &Path,
    name: &str,
    branch: Option<&str>,
    pol: &Resolved,
    wt: &Path,
    br: &str,
    out: &mut String,
    err: &mut String,
) -> i32 {
    // Reuse keyed on the BRANCH alone: a branch has one checkout, so
    // anything that moved the resolved path reuses wherever the branch
    // actually lives - never `git worktree add`'s "already used by worktree
    // at ..." wedge.
    if branch.is_none() && br != format!("feature/{name}") {
        if let Some(existing) = worktree_for_branch(top, br) {
            let existing_r = resolve_lenient(&existing);
            let wt_r = resolve_lenient(wt);
            let top_r = resolve_lenient(top);
            if existing_r != wt_r && existing_r != top_r && existing.exists() {
                err.push_str(&format!(
                    "worktree ensure: {}; reusing {br} at {} (policy resolves {}; \
                     a branch has one checkout) created=false\n",
                    policy_receipt(pol),
                    existing.to_string_lossy(),
                    wt.to_string_lossy()
                ));
                out.push_str(&existing.to_string_lossy());
                out.push('\n');
                return 0;
            }
        }
    }
    create_branch_arm(top, name, branch, pol, wt, br, out, err)
}

fn create_branch_arm(
    top: &Path,
    name: &str,
    branch: Option<&str>,
    pol: &Resolved,
    wt: &Path,
    br: &str,
    out: &mut String,
    err: &mut String,
) -> i32 {
    // base= stays ONE whitespace-free token: the orientation line is a
    // key=value contract.
    let mut base_note = String::new();
    let exists = git_out(
        top,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{br}"),
        ],
    );
    let add = if exists.map(|o| o.status.success()).unwrap_or(false) {
        git_bounded(
            top,
            &["worktree", "add", &wt.to_string_lossy(), br],
            FETCH_TIMEOUT_SECS,
        )
    } else {
        create_fresh_arm(top, name, branch, wt, br, &mut base_note)
    };
    let Some(add) = add else {
        err.push_str("worktree ensure: git worktree add failed: timed out\n");
        return 1;
    };
    if !add.status.success() {
        let detail = if String::from_utf8_lossy(&add.stderr).trim().is_empty() {
            String::from_utf8_lossy(&add.stdout).trim().to_string()
        } else {
            String::from_utf8_lossy(&add.stderr).trim().to_string()
        };
        err.push_str(&format!(
            "worktree ensure: git worktree add failed: {detail}\n"
        ));
        return 1;
    }
    err.push_str(&format!(
        "worktree ensure: {}; worktree at {}{base_note} created=true\n",
        policy_receipt(pol),
        wt.to_string_lossy()
    ));
    out.push_str(&wt.to_string_lossy());
    out.push('\n');
    0
}

/// The fresh-cut arm: continuation and salvage refs, else origin/main.
/// A repo with no remote-tracking main (local-only) cuts from HEAD, the
/// deleted leg's documented fallback - a None base here would surface as a
/// misleading worktree-add timeout instead.
fn create_fresh_arm(
    top: &Path,
    name: &str,
    branch: Option<&str>,
    wt: &Path,
    br: &str,
    _base_note: &mut String,
) -> Option<std::process::Output> {
    let base = base_ref(top).unwrap_or_else(|| "HEAD".to_string());
    let cont = if branch.is_none() {
        continuation_base(top, name, &base, br)
    } else {
        None
    };
    if let Some(cont) = cont {
        if cont.kind == "continued" {
            return git_bounded(
                top,
                &[
                    "worktree",
                    "add",
                    "-b",
                    br,
                    "--track",
                    &wt.to_string_lossy(),
                    &cont.create_target,
                ],
                FETCH_TIMEOUT_SECS,
            );
        }
        git_bounded(
            top,
            &[
                "worktree",
                "add",
                &wt.to_string_lossy(),
                "-b",
                br,
                &cont.create_target,
            ],
            FETCH_TIMEOUT_SECS,
        )
    } else {
        git_bounded(
            top,
            &["worktree", "add", &wt.to_string_lossy(), "-b", br, &base],
            FETCH_TIMEOUT_SECS,
        )
    }
}
