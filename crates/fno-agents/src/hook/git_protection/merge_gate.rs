//! The raw `gh pr merge` gate. Four vetoes run first, in this order: the
//! plan dispatch hold (fails closed), the stacked base, review coverage and
//! a running review (each fails open on its own machinery). Then the
//! two-factor allow: an active target session armed for auto-merge plus
//! external-review evidence. Last, the operator's single-use override
//! marker, recorded in an append-only log. `fno do pr merge` is the
//! sanctioned primitive and this hook does not gate it.

use super::segments::{base_lower, effective_argv, is_digits, split_words, Segment};
use super::Ctx;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// The lineage and coverage probes share one bound, and the hold read its
/// own: they run in series inside one 60 s PreToolUse budget, and a hook the
/// harness kills emits no verdict at all.
pub(crate) const VETO_PROBE_TIMEOUT: Duration = Duration::from_secs(24);
pub(crate) const HOLD_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Both markers expire: a forgotten sentinel must not hold a gate open.
const MARKER_TTL: Duration = Duration::from_secs(300);
const SESSION_FRESH: Duration = Duration::from_secs(3600);

fn merge_marker(home: &Path) -> PathBuf {
    home.join("merge-gate.disabled")
}

fn override_log(home: &Path) -> PathBuf {
    crate::state_layout::place(home, "merge-gate-overrides.log")
}

fn age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

/// True if `path` is younger than the TTL. An expired marker is removed.
pub(super) fn fresh_marker(path: &Path) -> bool {
    match age(path) {
        Some(a) if a < MARKER_TTL => true,
        Some(_) => {
            let _ = std::fs::remove_file(path);
            false
        }
        None => false,
    }
}

/// The unlink IS the claim: of two hooks that both saw a fresh marker, only
/// the one that removes it is authorized.
pub(super) fn claim_marker(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
}

fn is_merge(argv: &[String]) -> bool {
    argv.len() >= 3
        && base_lower(&argv[0]) == "gh"
        && argv[1].eq_ignore_ascii_case("pr")
        && argv[2].eq_ignore_ascii_case("merge")
}

/// Every `gh pr merge` segment, so one override cannot authorize three.
pub(super) fn find_merge_segments(segs: &[Segment]) -> Vec<String> {
    segs.iter()
        .filter(|seg| is_merge(&effective_argv(seg)))
        .map(|seg| seg.join(" "))
        .collect()
}

/// The fallback's count of merge mentions in an unsplittable command.
pub(super) fn merge_mentions(command: &str) -> usize {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)gh\s+pr\s+merge").expect("static regex"))
        .find_iter(command)
        .count()
}

/// A shell runner's `-c` argument is one segment however many commands it
/// holds, so it never stands alone.
pub(super) fn shell_wrapped(command: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\b(sh|bash|zsh|dash|ksh)\b[^|;&]*\s-\w*c\b").expect("static regex")
    })
    .is_match(command)
}

/// The PR number a `gh pr merge` names (a number or a /pull/<n> URL), else
/// None: the branch and current-branch forms cannot be read from text.
fn parse_merge_pr(command: &str) -> Option<String> {
    static URL: OnceLock<Regex> = OnceLock::new();
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let start = (0..tokens.len().saturating_sub(2)).find(|&i| {
        base_lower(tokens[i]) == "gh"
            && tokens[i + 1].eq_ignore_ascii_case("pr")
            && tokens[i + 2].eq_ignore_ascii_case("merge")
    })? + 3;
    let tok = tokens[start..].iter().find(|t| !t.starts_with('-'))?;
    if is_digits(tok) {
        return Some(tok.to_string());
    }
    URL.get_or_init(|| Regex::new(r"/pull/(\d+)/?$").expect("static regex"))
        .captures(tok)
        .map(|c| c[1].to_string())
}

/// True when the gh call names another repository (`-R`, `--repo`,
/// `GH_REPO=`, a PR URL). Every probe here reads this checkout, so such a
/// merge is unanswerable. Value-shaped, so a flag value quoting `-R` or a
/// pull URL in prose is not an override.
fn targets_other_repo(command: &str) -> bool {
    let tokens = split_words(command)
        .unwrap_or_else(|| command.split_whitespace().map(str::to_string).collect());
    for (i, t) in tokens.iter().enumerate() {
        let flag = t.starts_with("-R") || t.starts_with("--repo");
        if t.contains(' ') || (t.starts_with('-') && !flag) {
            continue;
        }
        if t.starts_with("GH_REPO=") {
            return true;
        }
        if t == "-R" || t == "--repo" {
            if tokens.get(i + 1).is_some_and(|next| !next.starts_with('-')) {
                return true;
            }
            continue;
        }
        if flag {
            let attached = if t.starts_with("--") {
                t.split_once('=').map_or(t.as_str(), |(_, v)| v)
            } else {
                &t[2..]
            };
            if attached.contains('/') && !attached.starts_with('-') {
                return true;
            }
            continue;
        }
        if t.contains("/pull/")
            && (t.starts_with("http://")
                || t.starts_with("https://")
                || t.starts_with("github.com/"))
        {
            return true;
        }
    }
    false
}

/// The probe's refusal line: the first line of its stderr, else stdout.
fn first_line(stderr: &str, stdout: &str) -> Option<String> {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    text.trim().lines().next().map(str::to_string)
}

/// Lineage and coverage: deny only on the door's refusal exit (3). A
/// timeout, a missing door or any other exit fails open, because a guard
/// whose own machinery is down must not become a merge outage.
fn open_veto(ctx: &Ctx, door: &str, pr: &str, fallback: String) -> Option<String> {
    match ctx.probes.veto(&["pr", door, pr], VETO_PROBE_TIMEOUT) {
        Ok((3, stderr, stdout)) => Some(first_line(&stderr, &stdout).unwrap_or(fallback)),
        _ => None,
    }
}

/// The plan dispatch hold, fail closed: an unanswerable hold is not unheld.
fn hold_refusal(command: &str, ctx: &Ctx) -> Option<String> {
    if targets_other_repo(command) {
        return Some(
            "cannot verify plan dispatch hold for a merge targeting another repository".into(),
        );
    }
    let Some(pr) = parse_merge_pr(command) else {
        return Some(
            "cannot determine which PR this bare `gh pr merge` targets, so its plan dispatch \
             hold cannot be checked; refusing to assume unheld. Run `fno do pr merge <N>` (or \
             pass the PR number), which resolves the PR and is not gated by this hook"
                .into(),
        );
    };
    match ctx
        .probes
        .veto(&["do", "pr", "hold-check", &pr], HOLD_PROBE_TIMEOUT)
    {
        Err(why) => Some(format!(
            "dispatch hold check unavailable ({why}); refusing to assume unheld"
        )),
        Ok((0, _, _)) => None,
        Ok((_, stderr, stdout)) => Some(
            first_line(&stderr, &stdout)
                .unwrap_or_else(|| format!("PR {pr}: dispatch hold state unreadable")),
        ),
    }
}

/// `%XX` decoding for the claim key in a lockfile name.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit)
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Any review-hold lockfile in this repo denies. A file read, not a probe:
/// the two probes above already spend most of the hook budget. Coarse in
/// the safe direction: a wrong deny costs one `fno do pr merge`.
fn review_hold_refusal(command: &str, ctx: &Ctx) -> Option<String> {
    if parse_merge_pr(command).is_none() || targets_other_repo(command) {
        return None;
    }
    let claims = ctx.probes.review_claims_dir()?;
    let mut held: Vec<String> = std::fs::read_dir(&claims)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("review%3Abranch%3A") && n.ends_with(".lock"))
        .collect();
    if held.is_empty() {
        return None;
    }
    held.sort();
    let names: Vec<String> = held
        .iter()
        .take(3)
        .map(|n| unquote(&n[..n.len() - ".lock".len()]))
        .collect();
    Some(format!(
        "a review is registered as in flight in this repo ({}); merging now can ship the code \
         the review is still fixing",
        names.join(", ")
    ))
}

/// The frontmatter of an active target session at `state`: fresh within
/// the hour and `status: IN_PROGRESS`. A candidate it cannot read is
/// skipped, never fatal: discovery spans every worktree.
fn active_state(state: &Path) -> Option<HashMap<String, String>> {
    if age(state)? > SESSION_FRESH {
        return None;
    }
    let text = match std::fs::read_to_string(state) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[git-protection] skip {}: {e}", state.display());
            return None;
        }
    };
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut fm = HashMap::new();
    for line in lines {
        let s = line.trim();
        if s == "---" {
            break;
        }
        if let Some((k, v)) = s.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            fm.insert(k.trim().to_string(), v.to_string());
        }
    }
    fm.get("status")
        .is_some_and(|s| s.eq_ignore_ascii_case("IN_PROGRESS"))
        .then_some(fm)
}

fn artifact_path(root: &Path, sid: &str) -> PathBuf {
    root.join(".fno")
        .join("artifacts")
        .join(format!("external-{sid}.md"))
}

fn artifact_pr(path: &Path) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let text = std::fs::read_to_string(path).ok()?;
    R.get_or_init(|| Regex::new(r#"(?m)^pr_number:\s*["']?(\d+)["']?\s*$"#).expect("static regex"))
        .captures(&text)
        .map(|c| c[1].to_string())
}

type Session = (PathBuf, HashMap<String, String>, PathBuf);

/// The one active session that authorizes this merge, failing closed on
/// ambiguity. A numbered PR binds to the worktree on its head branch; an
/// unnumbered merge needs exactly one active session anywhere.
fn active_session(prefer_pr: Option<&str>, ctx: &Ctx) -> Option<Session> {
    let roots = match prefer_pr {
        Some(pr) => ctx.probes.pr_worktree(pr).into_iter().collect(),
        None => ctx.probes.repo_roots(),
    };
    let matches: Vec<Session> = roots
        .into_iter()
        .filter_map(|root| {
            // The space slice first, the pre-space checkout manifest second.
            let state = crate::target_orient::manifest_path(&root)?;
            active_state(&state).map(|fm| (state, fm, root))
        })
        .collect();
    let Some(pr) = prefer_pr else {
        return (matches.len() == 1)
            .then(|| matches.into_iter().next())
            .flatten();
    };
    let mut neutral = Vec::new();
    for session in matches {
        let sid = session.1.get("session_id").map(|s| s.trim()).unwrap_or("");
        let recorded = (!sid.is_empty())
            .then(|| artifact_pr(&artifact_path(&session.2, sid)))
            .flatten();
        match recorded {
            Some(r) if r == pr => return Some(session),
            Some(_) => {}
            None => neutral.push(session),
        }
    }
    (neutral.len() == 1)
        .then(|| neutral.into_iter().next())
        .flatten()
}

fn truthy(v: Option<&String>) -> bool {
    v.is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "1"))
}

/// The two-factor allow reason, or None. The live switch must agree with
/// the manifest: a snapshot `true` never outlives the operator turning
/// auto-merge off, unless the run carries the spawn-time env grant.
fn two_factor_allows(command: &str, ctx: &Ctx) -> Option<String> {
    if targets_other_repo(command) {
        return None;
    }
    let pr = parse_merge_pr(command);
    let (state, fm, root) = active_session(pr.as_deref(), ctx)?;
    if !truthy(fm.get("auto_merge_approved")) {
        return None;
    }
    let env_grant = fm.get("auto_merge_source").map(|s| s.trim()) == Some("env-target-auto-merge");
    if !env_grant && !ctx.probes.merge_switch(&root) {
        return None;
    }
    let name = state.file_name()?.to_string_lossy().into_owned();
    if fm
        .get("external_review_passed")
        .is_some_and(|v| v.eq_ignore_ascii_case("skipped"))
        || truthy(fm.get("no_external"))
    {
        return Some(format!("active {name} (external review skipped by flag)"));
    }
    let sid = fm
        .get("session_id")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    let artifact = artifact_path(&root, sid);
    let artifact_age = age(&artifact)?;
    if artifact_age > SESSION_FRESH {
        return None;
    }
    let text = std::fs::read_to_string(&artifact).ok()?;
    let sid_re = Regex::new(&format!(
        r#"(?m)^session_id:\s*["']?{}["']?\s*$"#,
        regex::escape(sid)
    ))
    .ok()?;
    let phase_re = Regex::new(r#"(?m)^phase:\s*["']?external["']?\s*$"#).ok()?;
    if !sid_re.is_match(&text) || !phase_re.is_match(&text) {
        return None;
    }
    Some(format!(
        "active {name} + external review artifact ({}s old)",
        artifact_age.as_secs()
    ))
}

/// One line per consumed override, newlines flattened so a command cannot
/// forge entries. False when the line could not be written: the caller
/// refuses rather than allow an unrecorded override.
fn audit(message: &str, ctx: &Ctx) -> bool {
    use std::io::Write;
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    eprintln!("[Git Protection: merge-gate override] {flat}");
    let path = override_log(&ctx.home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{stamp} cwd={} {flat}", ctx.cwd.display()))
        .is_ok()
}

/// The merge verdict for one lone `gh pr merge` segment.
pub(super) fn judge_merge(seg: &str, parsed: bool, ctx: &Ctx) -> Option<String> {
    if let Some(why) = hold_refusal(seg, ctx) {
        return Some(format!("[fno agents dispatch-hold] {why}"));
    }
    let pr = parse_merge_pr(seg).filter(|_| !targets_other_repo(seg));
    if let Some(pr) = &pr {
        // Ahead of the two-factor allow and the override: a base that no
        // longer leads to the default branch ships nothing.
        let fallback = format!("PR {pr}: base no longer leads to the default branch");
        if let Some(why) = open_veto(ctx, "base-lineage-check", pr, fallback) {
            return Some(format!("[fno stacked-base] {why}"));
        }
        let fallback = format!("PR {pr}: review coverage refused");
        if let Some(why) = open_veto(ctx, "coverage-check", pr, fallback) {
            return Some(format!(
                "[fno do review-coverage] {why}\nRecovery: run `fno do pr merge`, which \
                 recomputes coverage and is not gated by this hook."
            ));
        }
    }
    if let Some(why) = review_hold_refusal(seg, ctx) {
        return Some(format!(
            "[fno review-hold] {why}\nRecovery: run `fno do pr merge`, which reads the precise \
             per-branch hold and is not gated by this hook. To clear a finished review: `fno \
             do pr review-hold release --branch <b> --holder <h>`."
        ));
    }
    if two_factor_allows(seg, ctx).is_some() {
        return None;
    }
    // The override needs parsed segments: only they prove the merge stands
    // alone. It is claimed only when it is what authorizes the merge.
    let marker = merge_marker(&ctx.home);
    if parsed && fresh_marker(&marker) && claim_marker(&marker) {
        if !audit(
            &format!("two-factor check failed, merge allowed by marker: {seg}"),
            ctx,
        ) {
            return Some(unrecordable_override_message(&ctx.home));
        }
        return None;
    }
    Some(merge_deny_message(seg, &ctx.home))
}

const RULE: &str = "═══════════════════════════════════════════════════════════════════";
const BOX_TOP: &str = "╔════════════════════════════════════════════════════════════════╗";
const BOX_BOTTOM: &str = "╚════════════════════════════════════════════════════════════════╝";

pub(super) fn compound_deny_message(command: &str) -> String {
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: one approval cannot authorize several actions
{BOX_BOTTOM}

Command: {command}

An approval here authorizes exactly ONE action, and a PreToolUse allow
applies to the WHOLE Bash call - so `gh pr merge 1 && <anything>` would
approve the anything too. This command carries more than the one action:

  • several `gh pr merge` or `--no-verify` segments, or both kinds
  • another command joined by && / ; / | (a leading `cd` counts too)
  • a command substitution: $(...), `...`, <(...) or >(...)
    A substitution IS a second command, so it disqualifies the
    approval even though it sits inside one segment.
  • an output redirection (>, >>) or an unquoted heredoc (<<EOF),
    which would ride the approval into a file write or an expansion

Run the approved action as its own substitution-free command. To pass a
long commit message without $(cat ...), use stdin instead:

  git commit --no-verify -F - <<'EOF'
  your message
  EOF

{RULE}
"
    )
}

fn unrecordable_override_message(home: &Path) -> String {
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: merge-gate override could not be recorded
{BOX_BOTTOM}

The marker was valid but the trail could not be written, and the trail
is what justifies having an override at all - so this is refused rather
than allowed unrecorded. Check that this path is an appendable file (not
a directory) with a writable parent:
  {}

The marker was consumed; fix the path, then touch it again:
  touch {}

{RULE}
",
        override_log(home).display(),
        merge_marker(home).display()
    )
}

fn merge_deny_message(command: &str, home: &Path) -> String {
    format!(
        "{BOX_TOP}
║  🚫 BLOCKED: gh pr merge (two-factor check failed)
{BOX_BOTTOM}

Command: {command}

Raw `gh pr merge` is gated at the shipping boundary. The sanctioned merge
primitive is `fno do pr merge`, which runs its own footnote-canonical guards
and is not blocked by this hook.

Auto-merge directly from Claude Code requires ALL of:
  1. The live merge switch armed: `fno config get auto_merge.enabled`
     answers true, OR the state file carries `auto_merge_source:
     env-target-auto-merge` (the run's spawn-time grant)
  2. Active target state file with `auto_merge_approved: true`
     (only the target manifest authorizes merge)
  3. Either:
     a. `external_review_passed: skipped` in state (explicit --no-external), OR
     b. External review artifact at
        <repo>/.fno/artifacts/external-<session_id>.md
        with matching frontmatter (phase: external, session_id: <sid>)

The artifact proves /fno:ship pr check actually ran for this session. A stale
or missing artifact blocks the merge even if the state flag is true.

Ahead of the two factors above sits a third veto: review coverage. A bare
`gh pr merge` also requires a `covered` review_coverage row pinned to the
PR's current head. A missing or stale row is refused here even when both
factors pass, because nothing reviewed the head that would merge. The
sanctioned primitive `fno do pr merge` recomputes that row itself.

If /fno:ship pr check was skipped or failed, the correct recovery is to run it
again or explicitly configure --no-external. Do not forge the artifact.

⚠️  Operator escape hatch, when the two-factor path is genuinely broken
    (e.g. an immutable manifest carrying the wrong value):

  touch {}

    `gh pr merge` only: it cannot open a push to a protected branch and
    cannot open --no-verify. Expires in 5 minutes, consumed on use, every
    use appended to {}. An agent must not create this
    unprompted - that is a logged violation, not a silent bypass.

{RULE}
",
        merge_marker(home).display(),
        override_log(home).display()
    )
}

#[cfg(test)]
mod tests {
    use super::super::fake::{bash, Fake};
    use super::super::{judge_with, Ctx, Exec};
    use super::*;

    fn ctx<'a>(dir: &Path, fake: &'a Fake) -> Ctx<'a> {
        Ctx {
            home: dir.join("home"),
            cwd: dir.to_path_buf(),
            probes: fake,
        }
    }

    fn refuse(code: i32, line: &str) -> Exec {
        Ok((code, line.to_string(), String::new()))
    }

    /// A worktree with an active session armed for auto-merge, its
    /// external artifact recording `pr`.
    fn session(root: &Path, pr: &str, extra: &str) {
        let fno = root.join(".fno");
        std::fs::create_dir_all(fno.join("artifacts")).unwrap();
        std::fs::write(
            fno.join("target-state.md"),
            format!(
                "---\nstatus: IN_PROGRESS\nsession_id: s1\nauto_merge_approved: true\n{extra}---\n"
            ),
        )
        .unwrap();
        std::fs::write(
            fno.join("artifacts").join("external-s1.md"),
            format!("---\nphase: external\nsession_id: s1\npr_number: {pr}\n---\n"),
        )
        .unwrap();
    }

    #[test]
    fn the_hold_veto_fails_closed_and_runs_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::default();
        fake.vetoes
            .insert("hold-check", refuse(3, "PR 42: plan x-1 is held"));
        let c = ctx(dir.path(), &fake);
        let r = judge_with(&bash("gh pr merge 42"), &c).unwrap();
        assert_eq!(r, "[fno agents dispatch-hold] PR 42: plan x-1 is held");
        assert_eq!(
            *fake.calls.borrow(),
            ["hold-check"],
            "nothing after the hold ran"
        );
        let r = judge_with(&bash("gh pr merge --squash"), &c).unwrap();
        assert!(r.contains("cannot determine which PR"), "{r}");
        let r = judge_with(&bash("gh pr merge 42 --repo o/other"), &c).unwrap();
        assert!(r.contains("another repository"), "{r}");
        let mut down = Fake::default();
        down.vetoes.insert("hold-check", Err("timeout".into()));
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &down)).unwrap();
        assert!(r.contains("unavailable (timeout)"), "{r}");
    }

    #[test]
    fn lineage_and_coverage_refuse_only_on_exit_3() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::default();
        fake.vetoes.insert(
            "base-lineage-check",
            refuse(3, "base-lineage: REFUSED - stale"),
        );
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).unwrap();
        assert_eq!(r, "[fno stacked-base] base-lineage: REFUSED - stale");
        let mut fake = Fake::default();
        fake.vetoes
            .insert("base-lineage-check", refuse(4, "unknown"));
        fake.vetoes.insert("coverage-check", refuse(3, ""));
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).unwrap();
        assert!(
            r.starts_with("[fno do review-coverage] PR 42: review coverage refused\nRecovery"),
            "{r}"
        );
        let mut fake = Fake::default();
        fake.vetoes.insert("coverage-check", Err("timeout".into()));
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).unwrap();
        assert!(
            r.contains("two-factor check failed"),
            "an open veto falls to the gate: {r}"
        );
    }

    #[test]
    fn a_running_review_blocks_the_merge() {
        let dir = tempfile::tempdir().unwrap();
        let claims = dir.path().join(".fno").join("claims");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::write(claims.join("review%3Abranch%3Afeature%2Fx.lock"), b"").unwrap();
        let fake = Fake {
            claims_dir: Some(claims.clone()),
            ..Fake::default()
        };
        let r = judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).unwrap();
        assert!(r.starts_with("[fno review-hold] a review is registered as in flight in this repo (review:branch:feature/x)"), "{r}");
    }

    #[test]
    fn two_factor_allows_only_the_bound_armed_session() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        session(&wt, "42", "");
        let mut fake = Fake {
            pr_worktree: Some(wt.clone()),
            roots: vec![wt.clone()],
            switch: true,
            ..Fake::default()
        };
        assert_eq!(
            judge_with(&bash("gh pr merge 42 --squash"), &ctx(dir.path(), &fake)),
            None
        );
        // A session bound to another PR, a disarmed switch and a compound
        // command never authorize.
        assert!(judge_with(&bash("gh pr merge 43"), &ctx(dir.path(), &fake)).is_some());
        assert!(judge_with(&bash("gh pr merge 42 && rm -rf x"), &ctx(dir.path(), &fake)).is_some());
        assert!(judge_with(
            &bash("gh pr merge 42 --body \"`id`\""),
            &ctx(dir.path(), &fake)
        )
        .is_some());
        fake.switch = false;
        assert!(judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)).is_some());
        session(&wt, "42", "auto_merge_source: env-target-auto-merge\n");
        assert_eq!(
            judge_with(&bash("gh pr merge 42"), &ctx(dir.path(), &fake)),
            None
        );
    }

    #[test]
    fn the_override_marker_is_single_use_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let c = ctx(dir.path(), &fake);
        std::fs::create_dir_all(&c.home).unwrap();
        std::fs::write(merge_marker(&c.home), b"").unwrap();
        assert_eq!(judge_with(&bash("gh pr merge 42"), &c), None);
        assert!(!merge_marker(&c.home).exists());
        let log = std::fs::read_to_string(override_log(&c.home)).unwrap();
        assert!(
            log.contains("merge allowed by marker: gh pr merge 42"),
            "{log}"
        );
        assert!(judge_with(&bash("gh pr merge 42"), &c)
            .unwrap()
            .contains("two-factor check failed"));
        // One marker never covers two merges.
        std::fs::write(merge_marker(&c.home), b"").unwrap();
        let r = judge_with(&bash("gh pr merge 1; gh pr merge 2"), &c).unwrap();
        assert!(
            r.contains("one approval cannot authorize several actions"),
            "{r}"
        );
        // An unwritable trail refuses rather than allow unrecorded.
        std::fs::remove_file(override_log(&c.home)).unwrap();
        std::fs::create_dir_all(override_log(&c.home)).unwrap();
        let r = judge_with(&bash("gh pr merge 42"), &c).unwrap();
        assert!(r.contains("override could not be recorded"), "{r}");
    }

    #[test]
    fn merge_text_parsing() {
        assert_eq!(
            parse_merge_pr("/usr/bin/gh pr merge --squash 356").as_deref(),
            Some("356")
        );
        assert_eq!(
            parse_merge_pr("gh pr merge https://github.com/o/r/pull/7").as_deref(),
            Some("7")
        );
        assert_eq!(parse_merge_pr("gh pr merge feature/x"), None);
        assert!(targets_other_repo("gh pr merge 1 -Ro/r"));
        assert!(targets_other_repo("GH_REPO=o/r gh pr merge 1"));
        assert!(!targets_other_repo(
            "gh pr merge 1 -t \"-R fixes the deref\""
        ));
        assert!(!targets_other_repo("gh pr merge 1 --body 'See /pull/123'"));
        assert_eq!(unquote("review%3Abranch%3Ax"), "review:branch:x");
        assert_eq!(VETO_PROBE_TIMEOUT.as_secs(), 24);
    }
}
