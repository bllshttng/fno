//! The git-protection Bash guard. It refuses a push to a protected branch,
//! `--no-verify` and its core.hooksPath twin, a raw `gh pr merge` without
//! two-factor evidence, a `gh pr create` with no composed closure line, a
//! direct GraphQL PR read, and a push while a GitHub run is in flight.
//!
//! A PreToolUse allow approves the whole Bash call, so anything this guard
//! authorizes (a merge, an approved `--no-verify`) must stand alone. Its
//! state lives under `${FNO_HOME:-~/.fno}`. Git reads run in the process
//! cwd, never the payload cwd.

mod gh_gates;
mod git_gate;
mod merge_gate;
mod segments;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Child result of one probe: `(exit code, stderr, stdout)`, or the reason
/// the child could not answer (a timeout, a start failure).
pub(super) type Exec = Result<(i32, String, String), String>;

/// Every read the guard makes outside its own state files. The real side
/// runs git and this binary's own doors; tests answer from a fake.
pub(super) trait Probes {
    fn current_branch(&self) -> Option<String>;
    fn repo_roots(&self) -> Vec<PathBuf>;
    fn pr_worktree(&self, pr: &str) -> Option<PathBuf>;
    fn git_common_dir(&self) -> Option<PathBuf>;
    fn branch_node_ids(&self, head_ref: &str) -> Vec<String>;
    fn in_flight(&self, branch: &str) -> Option<Value>;
    fn merge_switch(&self, repo_root: &Path) -> bool;
    /// One veto door of this binary (`do pr hold-check`,
    /// `pr base-lineage-check`, `pr coverage-check`), bounded by `timeout`.
    fn veto(&self, args: &[&str], timeout: Duration) -> Exec;
    fn push_stamps_dir(&self) -> PathBuf;
    fn emit_push_bypass(&self, branch: &str);
}

pub(super) struct Ctx<'a> {
    /// `${FNO_HOME:-~/.fno}`: the root of the guard's state.
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub probes: &'a dyn Probes,
}

pub fn run(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let payload: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match judge(&payload, &cwd) {
        Some(reason) => super::emit_block(&reason),
        None => super::emit_allow(),
    }
}

/// The verdict for one payload: a refusal, or None to allow.
pub(super) fn judge(payload: &Value, cwd: &Path) -> Option<String> {
    let probes = RealProbes {
        cwd: cwd.to_path_buf(),
    };
    let ctx = Ctx {
        home: fno_home(),
        cwd: cwd.to_path_buf(),
        probes: &probes,
    };
    judge_with(payload, &ctx)
}

fn fno_home() -> PathBuf {
    match std::env::var_os("FNO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".fno"))
            .unwrap_or_else(|| PathBuf::from(".fno")),
    }
}

pub(super) fn judge_with(payload: &Value, ctx: &Ctx) -> Option<String> {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let command = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())?;
    // On an open quote command position is unknowable: every gate falls back
    // to a deny-leaning read of the whole string.
    let segments = segments::command_segments(command).ok();

    let reads = match &segments {
        Some(segs) => gh_gates::graphql_reads(segs),
        None => gh_gates::graphql_reads_fallback(command),
    };
    if let Some((kind, pr)) = reads.first() {
        return Some(gh_gates::graphql_reason(kind, pr));
    }

    if let Some(reason) = gh_gates::closure_refusal(command, segments.as_deref(), ctx) {
        return Some(reason);
    }

    let (merge_segs, git_segs) = match &segments {
        Some(segs) => (
            merge_gate::find_merge_segments(segs),
            git_gate::find_git_segments(segs),
        ),
        None => (
            vec![command.to_string(); merge_gate::merge_mentions(command)],
            if git_gate::mentions_git(command) {
                vec![command.to_string()]
            } else {
                Vec::new()
            },
        ),
    };
    let has_approval = merge_gate::fresh_marker(&git_gate::approval_flag(&ctx.home));

    // A deny writes state, so evaluation stops at the first one. A deny
    // anywhere outranks an allow anywhere: one approval never carries a
    // sibling segment another gate refuses.
    let mut git_allows: Vec<(String, String)> = Vec::new();
    for seg in &git_segs {
        match git_gate::evaluate(seg, has_approval, segments.is_some(), ctx) {
            Some(git_gate::Decision::Deny(reason)) => return Some(reason),
            Some(git_gate::Decision::Allow(reason)) => git_allows.push((seg.clone(), reason)),
            None => {}
        }
    }
    let merge_seg = merge_segs.first().cloned();
    let git_allow = git_allows.first().cloned();

    // One authorization authorizes one action.
    if merge_segs.len() > 1 || git_allows.len() > 1 || (merge_seg.is_some() && git_allow.is_some())
    {
        return Some(merge_gate::compound_deny_message(command));
    }
    match &segments {
        // Nothing can be counted on the fallback, so a single-use approval
        // authorizes nothing there. The merge gate keeps its own path.
        None if git_allow.is_some() => return Some(merge_gate::compound_deny_message(command)),
        None => {}
        Some(segs) => {
            let tokens: Vec<String> = segs.iter().flatten().cloned().collect();
            let carries_substitution = tokens
                .iter()
                .any(|t| ["`", "$(", "<(", ">("].iter().any(|f| t.contains(f)))
                || segments::writes_a_file(&tokens)
                || segments::has_unquoted_heredoc(command);
            let alone =
                segs.len() == 1 && !carries_substitution && !merge_gate::shell_wrapped(command);
            if (merge_seg.is_some() || git_allow.is_some()) && !alone {
                return Some(merge_gate::compound_deny_message(command));
            }
        }
    }

    if let Some(seg) = merge_seg {
        return merge_gate::judge_merge(&seg, segments.is_some(), ctx);
    }
    if let Some((seg, _)) = git_allow {
        // The claim must win: two hooks can both see a fresh flag, and only
        // the one that removes it is authorized.
        if !merge_gate::claim_marker(&git_gate::approval_flag(&ctx.home)) {
            return Some(git_gate::no_verify_deny_message(&seg, &ctx.home));
        }
    }
    None
}

struct RealProbes {
    cwd: PathBuf,
}

impl RealProbes {
    fn git(&self, args: &[&str], secs: u64) -> Option<String> {
        let out = crate::loopcheck::bounded_read_unadmitted(
            "git".as_ref(),
            args,
            &self.cwd,
            "git-protection",
            Duration::from_secs(secs),
        )
        .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn exe(&self, args: &[&str], timeout: Duration) -> Exec {
        let exe = std::env::current_exe().map_err(|e| format!("no binary path ({e})"))?;
        crate::loopcheck::bounded_read_unadmitted(
            exe.as_os_str(),
            args,
            &self.cwd,
            "git-protection",
            timeout,
        )
        .map(|out| {
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr_tail).into_owned(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        })
        .map_err(|e| e.outcome().to_string())
    }
}

impl Probes for RealProbes {
    fn current_branch(&self) -> Option<String> {
        self.git(&["symbolic-ref", "--short", "HEAD"], 1)
            .map(|out| out.trim().to_string())
            .filter(|b| !b.is_empty())
    }

    fn repo_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut add = |p: &str| {
            let path = PathBuf::from(p);
            let path = path.canonicalize().unwrap_or(path);
            if !roots.contains(&path) {
                roots.push(path);
            }
        };
        if let Some(top) = self.git(&["rev-parse", "--show-toplevel"], 1) {
            if !top.trim().is_empty() {
                add(top.trim());
            }
        }
        if let Some(list) = self.git(&["worktree", "list", "--porcelain"], 2) {
            for line in list.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    add(path.trim());
                }
            }
        }
        roots
    }

    fn pr_worktree(&self, pr: &str) -> Option<PathBuf> {
        let worktree = crate::pr_worktree::resolve_pr_with_timeout(
            &self.cwd,
            pr,
            "gh",
            Duration::from_secs(1),
        )
        .ok()?;
        worktree
            .is_dir()
            .then(|| worktree.canonicalize().unwrap_or(worktree))
    }

    fn git_common_dir(&self) -> Option<PathBuf> {
        // git under 2.31 echoes --path-format=absolute back and exits 0, so
        // only one absolute line counts; else the plain read joins the cwd.
        for flags in [&["--path-format=absolute"][..], &[][..]] {
            let mut args = vec!["rev-parse"];
            args.extend_from_slice(flags);
            args.push("--git-common-dir");
            let out = self.git(&args, 5)?;
            let lines: Vec<&str> = out.lines().collect();
            if lines.len() == 1 && (flags.is_empty() || Path::new(lines[0]).is_absolute()) {
                return Some(self.cwd.join(lines[0]));
            }
        }
        None
    }

    fn branch_node_ids(&self, head_ref: &str) -> Vec<String> {
        let payload = serde_json::json!({"head_ref": head_ref, "verify": true});
        let (code, stdout, stderr) = crate::org_board::pr_closure_produce::leg_branch_ids(&payload);
        let ids = (code == 0)
            .then(|| serde_json::from_str::<Value>(&stdout).ok())
            .flatten()
            .and_then(|v| v.get("ids").and_then(Value::as_array).cloned());
        match ids {
            Some(ids) => ids
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            None => {
                let cause = stderr.trim();
                let cause = if cause.is_empty() {
                    format!("exit {code}")
                } else {
                    cause.to_string()
                };
                eprintln!(
                    "[fno closure trailer] could not read branch node ids ({cause}); \
                     run fno doctor update --rust"
                );
                Vec::new()
            }
        }
    }

    fn in_flight(&self, branch: &str) -> Option<Value> {
        let (_, _, stdout) = self
            .exe(
                &["do", "pr", "push", "--in-flight", branch],
                Duration::from_secs(15),
            )
            .ok()?;
        let line = stdout
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{'))?;
        serde_json::from_str::<Value>(line)
            .ok()
            .filter(Value::is_object)
    }

    fn merge_switch(&self, repo_root: &Path) -> bool {
        crate::agents_config::auto_merge_enabled(repo_root)
    }

    fn veto(&self, args: &[&str], timeout: Duration) -> Exec {
        self.exe(args, timeout)
    }

    fn push_stamps_dir(&self) -> PathBuf {
        crate::pr_push::default_stamps_dir()
    }

    fn emit_push_bypass(&self, branch: &str) {
        let payload = serde_json::json!({ "branch": branch }).to_string();
        let _ = crate::loopcheck::bounded_read_unadmitted(
            "fno".as_ref(),
            &[
                "doctor",
                "event",
                "emit",
                "push_debounce_bypass",
                "--json",
                &payload,
            ],
            &self.cwd,
            "git-protection",
            Duration::from_secs(10),
        );
    }
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A scripted world: every field answers its probe, and `calls` records
    /// each veto door the guard asked.
    #[derive(Default)]
    pub struct Fake {
        pub branch: Option<String>,
        pub roots: Vec<PathBuf>,
        pub pr_worktree: Option<PathBuf>,
        pub common_dir: Option<PathBuf>,
        pub node_ids: Vec<String>,
        pub in_flight: Option<Value>,
        pub switch: bool,
        pub vetoes: HashMap<&'static str, Exec>,
        pub stamps: PathBuf,
        pub calls: RefCell<Vec<String>>,
    }

    impl Probes for Fake {
        fn current_branch(&self) -> Option<String> {
            self.branch.clone()
        }
        fn repo_roots(&self) -> Vec<PathBuf> {
            self.roots.clone()
        }
        fn pr_worktree(&self, _pr: &str) -> Option<PathBuf> {
            self.pr_worktree.clone()
        }
        fn git_common_dir(&self) -> Option<PathBuf> {
            self.common_dir.clone()
        }
        fn branch_node_ids(&self, _head_ref: &str) -> Vec<String> {
            self.node_ids.clone()
        }
        fn in_flight(&self, _branch: &str) -> Option<Value> {
            self.in_flight.clone()
        }
        fn merge_switch(&self, _repo_root: &Path) -> bool {
            self.switch
        }
        fn veto(&self, args: &[&str], _timeout: Duration) -> Exec {
            let door = args[args.len() - 2];
            self.calls.borrow_mut().push(door.to_string());
            self.vetoes
                .get(door)
                .cloned()
                .unwrap_or_else(|| Ok((0, String::new(), String::new())))
        }
        fn push_stamps_dir(&self) -> PathBuf {
            self.stamps.clone()
        }
        fn emit_push_bypass(&self, _branch: &str) {
            self.calls.borrow_mut().push("bypass".to_string());
        }
    }

    pub fn bash(command: &str) -> Value {
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}})
    }
}
