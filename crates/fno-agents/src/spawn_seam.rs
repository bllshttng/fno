//! The spawn seam, in process. Every `spawn` argv passes here before the
//! client parses it: [`crate::spawn_admission::admit`] (the seed refusals, the
//! node seed rewrite, normalization, the compose and the account pick), then
//! the duties the Python front door ran around it before it exec'd this
//! client: the codex git-grant refusal, the loop gate, the worker
//! writable-dir export, the codex sandbox probe and the worktree-policy pin.
//! The seam does NOT scrub the ambient identity markers: the parent-edge
//! capture downstream (`build_request`'s spawn arm, the ask lanes' mints)
//! reads them, and every child launch scrubs its own command instead. A
//! non-spawn argv passes through untouched.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::spawn_admission::{flag_value, has_flag, seed_of, HARNESS_FLAGS};

/// The env carrier the worker argv builders read the write grants from.
pub const WORKER_ADD_DIRS_ENV: &str = "FNO_WORKER_ADD_DIRS";

/// The spawn gate's exit for a sandbox that blocks a tool the worker needs.
pub const EXIT_SANDBOX_UNREACHABLE: i32 = 85;

/// Admit one argv. `Ok` is the argv to parse; `Err` is the exit code of a
/// refusal whose lines are already printed.
pub fn admit_spawn(args: Vec<String>) -> Result<Vec<String>, i32> {
    if args.first().map(String::as_str) != Some("spawn") {
        return Ok(args);
    }
    if let Some(admitted) = strip_admitted_marker(&args) {
        return Ok(admitted);
    }
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    refuse_codex_without_git_grant(&args, &here)?;

    let home = crate::paths::AgentsHome::from_env();
    let names: HashSet<String> = crate::state::load_registry(&home.registry_json())
        .map(|r| r.entries.into_iter().map(|e| e.name).collect())
        .unwrap_or_default();
    let admitted = crate::spawn_admission::admit(&args, &here, &names);
    for line in &admitted.stderr {
        eprintln!("{line}");
    }
    if let Some(stdout) = &admitted.stdout {
        println!("{}", crate::research::pyfmt::json_dumps(stdout));
    }
    if let Some(code) = admitted.exit {
        return Err(code);
    }
    for event in &admitted.events {
        let fields: Vec<(&str, serde_json::Value)> = event
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        crate::claude_ask::emit_event(&home.events_jsonl(), "model_vendor_mismatch", &fields);
    }
    let argv = admitted.argv;
    if let Some(refusal) = gate_seed(&argv)
        .and_then(|seed| crate::capability_leaves::loop_refusal(&spawn_harness(&argv), &seed, None))
    {
        eprintln!("{refusal}");
        return Err(2);
    }

    let cwd = spawn_cwd(&argv, &here);
    export_worker_dirs(&cwd);
    refuse_codex_unreachable_tools(&argv, &cwd)?;
    pin_worktree_policy(&argv, &here);
    // The ambient harness rides the argv explicitly, as the Python leg passed
    // it: the row's harness is decided once here, not re-derived later from
    // an env a child launch may have re-stamped.
    let mut argv = argv;
    if flag_value(&argv[1..], HARNESS_FLAGS).is_none() {
        let harness = spawn_harness(&argv);
        argv.splice(1..1, ["--harness".to_string(), harness]);
    }
    Ok(argv)
}

/// The Python spawn bridges (`rust_spawn.py`) build an argv their caller
/// already composed and stamp it `--defaults-applied`. Such an argv skips
/// admission, minus the marker. Only the fno-arg head is read: past the
/// `--argv` payload or the `--` fence the token is the worker's own text.
fn strip_admitted_marker(args: &[String]) -> Option<Vec<String>> {
    let head = args
        .iter()
        .position(|a| a == "--argv" || a == "--")
        .unwrap_or(args.len());
    let at = args[..head]
        .iter()
        .position(|a| a == "--defaults-applied" || a.starts_with("--defaults-applied="))?;
    let mut out = args.to_vec();
    out.remove(at);
    Some(out)
}

/// The spawn's effective working directory: `--cwd`/`-c`, else the caller's.
fn spawn_cwd(args: &[String], here: &Path) -> PathBuf {
    flag_value(&args[1..], &["--cwd", "-c"])
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| here.to_path_buf())
}

/// The seed the loop gate judges: the inline message, else the contents of a
/// `--prompt-file` path. `-` (stdin) is left for the parse to read once.
fn gate_seed(args: &[String]) -> Option<String> {
    seed_of(&args[1..]).or_else(|| {
        flag_value(&args[1..], &["--prompt-file"])
            .filter(|p| !p.is_empty() && p != "-")
            .and_then(|p| std::fs::read_to_string(p).ok())
    })
}

/// The harness a spawn dispatches to: the flag, else the ambient harness,
/// else claude.
fn spawn_harness(args: &[String]) -> String {
    flag_value(&args[1..], HARNESS_FLAGS)
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .or_else(|| crate::claims::resolve_harness().map(|h| h.to_lowercase()))
        .unwrap_or_else(|| "claude".to_string())
}

/// A codex spawn whose payload is a code workflow: a `--node` dispatch, or a
/// token whose first word names a fno command. The scan reads every token,
/// the fenced seed included.
fn is_codex_code_payload(args: &[String]) -> bool {
    let tail = &args[1..];
    if flag_value(tail, &["--node"]).is_some_and(|n| !n.is_empty()) {
        return true;
    }
    let mut i = 0;
    while i < tail.len() {
        let mut token = tail[i].as_str();
        if token == "--message" && i + 1 < tail.len() {
            token = tail[i + 1].as_str();
        } else if let Some(v) = token.strip_prefix("--message=") {
            token = v;
        }
        if token
            .split_whitespace()
            .next()
            .is_some_and(|w| crate::pane_spawn::codex::CODE_PAYLOAD_PREFIXES.contains(&w))
        {
            return true;
        }
        i += 1;
    }
    false
}

/// A codex code spawn that will run under a bounded sandbox. A bypass
/// posture, a promoted spawn and the headless lane run with full access, so
/// neither the git grant nor the probe applies to them.
fn is_bounded_codex_code_spawn(args: &[String]) -> bool {
    let tail = &args[1..];
    if spawn_harness(args) != "codex" || !is_codex_code_payload(args) {
        return false;
    }
    if has_flag(
        args,
        "-Y",
        &["--yolo", "--dangerously-bypass-approvals-and-sandbox"],
    ) {
        return false;
    }
    let promoted = has_flag(args, "", &["--promote"])
        || (flag_value(tail, &["--role-level"]).is_some_and(|v| !v.is_empty())
            && flag_value(tail, &["--role-scope"]).is_some_and(|v| !v.is_empty()));
    if promoted {
        return false;
    }
    let headless = has_flag(args, "", &["--once"])
        || flag_value(tail, &["--substrate"])
            .is_some_and(|s| s.trim().to_lowercase() == "headless");
    if headless {
        return false;
    }
    let sandbox = flag_value(tail, &["--permission-mode"]).unwrap_or_default();
    let sandbox = sandbox
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    !["yolo", "bypasspermissions", "danger-full-access"].contains(&sandbox.as_str())
}

/// A bounded codex code spawn with no git common dir refuses before any row.
fn refuse_codex_without_git_grant(args: &[String], here: &Path) -> Result<(), i32> {
    if !is_bounded_codex_code_spawn(args) {
        return Ok(());
    }
    let cwd = spawn_cwd(args, here);
    if crate::provider::git_common_dir(&cwd).is_some() {
        return Ok(());
    }
    eprintln!(
        "fno agents spawn: Codex code payloads require a resolved git grant; cwd={}; \
         git rev-parse --git-common-dir failed; no worker launched",
        cwd.to_string_lossy()
    );
    Err(2)
}

/// A bounded codex code spawn whose sandbox blocks a tool it needs refuses
/// before any row. An unjudged sandbox launches with a named caveat.
fn refuse_codex_unreachable_tools(args: &[String], cwd: &Path) -> Result<(), i32> {
    if !is_bounded_codex_code_spawn(args) {
        return Ok(());
    }
    let mode = flag_value(&args[1..], &["--permission-mode"]);
    let probe = crate::sandbox_probe::probe_codex_sandbox(cwd, mode.as_deref(), &[]);
    if probe.verdict == "unknown" {
        eprintln!(
            "sandbox-probe: could not judge the codex sandbox ({}); launching unprobed",
            probe.note
        );
    }
    for (tool, evidence) in &probe.blocked {
        eprintln!(
            "sandbox-probe: {tool} is unreachable inside the codex workspace-write sandbox \
             ({evidence}); no worker launched, node stays dispatchable"
        );
    }
    if probe.blocked.is_empty() {
        return Ok(());
    }
    eprintln!(
        "remedy: allow it in ~/.codex/config.toml (gh needs [sandbox_workspace_write] \
         network_access = true; git needs the git common dir writable), or dispatch \
         unsandboxed with --yolo"
    );
    Err(EXIT_SANDBOX_UNREACHABLE)
}

/// The write grants a worker spawned at `cwd` needs: the state root, the
/// claim store's root, the two registry directories, and this spawn's plan
/// directory. Absolute, deduplicated, existing only, in that order.
pub fn worker_writable_dirs(cwd: &Path) -> Vec<String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    roots.extend(crate::agents_config::state_dir(cwd));
    roots.extend(
        crate::claims_root::global_claims_dir().and_then(|d| d.parent().map(Path::to_path_buf)),
    );
    let home = crate::paths::AgentsHome::from_env();
    for dir in [
        home.registry_json().parent().map(Path::to_path_buf),
        crate::agents_config::state_dir(cwd).map(|s| s.join("agents")),
    ]
    .into_iter()
    .flatten()
    {
        if !roots.iter().any(|root| dir.starts_with(root)) {
            roots.push(dir);
        }
    }
    roots.extend(crate::plans_path::plans_content_dir(cwd));
    let mut out: Vec<String> = Vec::new();
    for cand in roots {
        let Ok(resolved) = cand.canonicalize() else {
            continue;
        };
        if !resolved.is_dir() {
            continue;
        }
        let token = resolved.to_string_lossy().into_owned();
        if !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

/// Publish the grants for the worker argv builders. An empty set CLEARS the
/// carrier: a spawn made from inside a worker inherits its parent's value,
/// another project's directories, which must never ride as this one's grant.
fn export_worker_dirs(cwd: &Path) {
    let dirs = worker_writable_dirs(cwd);
    if dirs.is_empty() {
        std::env::remove_var(WORKER_ADD_DIRS_ENV);
    } else {
        let sep = if cfg!(windows) { ";" } else { ":" };
        std::env::set_var(WORKER_ADD_DIRS_ENV, dirs.join(sep));
    }
}

/// A `--cwd` naming a repo no project declares pins the worktree policy to
/// `never` for this spawn and prints the receipt. An operator-set policy wins.
fn pin_worktree_policy(args: &[String], here: &Path) {
    if std::env::var_os("FNO_WORKTREE_POLICY").is_some_and(|v| !v.is_empty()) {
        return;
    }
    let tail = &args[1..];
    let Some(target) = flag_value(tail, &["--cwd", "-c"]).filter(|c| !c.is_empty()) else {
        return;
    };
    let harness = flag_value(tail, HARNESS_FLAGS).unwrap_or_default();
    let pinned =
        crate::worktree::policy::undeclared_dispatch_pin(Path::new(&target), here, Some(&harness));
    if pinned.is_some() {
        eprintln!("{}", crate::worktree::policy::UNDECLARED_REPO_RECEIPT);
        std::env::set_var("FNO_WORKTREE_POLICY", "never");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn only_a_bounded_codex_code_spawn_needs_the_grant_and_the_probe() {
        let code = |extra: &[&str]| {
            let mut t = vec!["spawn", "w", "-H", "codex"];
            t.extend_from_slice(extra);
            t.extend_from_slice(&["--", "/fno:target x-1"]);
            is_bounded_codex_code_spawn(&argv(&t))
        };
        assert!(code(&[]));
        assert!(!code(&["--yolo"]));
        assert!(!code(&["--promote"]));
        assert!(!code(&["--once"]));
        assert!(!code(&["--permission-mode", "danger-full-access"]));
        assert!(is_bounded_codex_code_spawn(&argv(&[
            "spawn", "w", "-H", "codex", "--node", "x-1", "--", "hi"
        ])));
        assert!(!is_bounded_codex_code_spawn(&argv(&[
            "spawn", "w", "-H", "codex", "--", "say hi"
        ])));
        assert!(!is_bounded_codex_code_spawn(&argv(&[
            "spawn",
            "w",
            "-H",
            "claude",
            "--",
            "/fno:target x-1"
        ])));

        // A bridge's marked argv skips admission, minus the marker; a fenced
        // marker is the worker's text and admits as usual.
        assert_eq!(
            strip_admitted_marker(&argv(&[
                "spawn",
                "--defaults-applied=unenforced",
                "-H",
                "codex"
            ])),
            Some(argv(&["spawn", "-H", "codex"]))
        );
        assert_eq!(
            strip_admitted_marker(&argv(&["spawn", "w", "--", "--defaults-applied"])),
            None
        );
    }

    #[test]
    fn the_loop_gate_judges_a_prompt_file_seed_but_never_reads_stdin() {
        let file = std::env::temp_dir().join(format!("fno-gate-seed-{}.txt", std::process::id()));
        std::fs::write(&file, "/fno:target x-1").unwrap();
        let path = file.to_string_lossy().into_owned();
        assert_eq!(
            gate_seed(&argv(&["spawn", "--name", "w", "--prompt-file", &path])).as_deref(),
            Some("/fno:target x-1")
        );
        assert_eq!(
            gate_seed(&argv(&[
                "spawn",
                "--name",
                "w",
                "--prompt-file",
                &path,
                "--",
                "hi"
            ]))
            .as_deref(),
            Some("hi")
        );
        assert_eq!(
            gate_seed(&argv(&["spawn", "--name", "w", "--prompt-file", "-"])),
            None
        );
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn admit_spawn_keeps_the_callers_identity_markers() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!("fno-seam-identity-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let _home = crate::claims::EnvVarGuard::set("FNO_AGENTS_HOME", home.to_str().unwrap());
        let _thread = crate::claims::EnvVarGuard::set("CODEX_THREAD_ID", "01a0bba2-parent");
        let _claude =
            crate::claims::EnvVarGuard::set("CLAUDE_CODE_SESSION_ID", "seed-2222-3333");

        let admitted =
            admit_spawn(argv(&["spawn", "--name", "wk", "hi", "--harness", "claude"]));
        // Read before the guards restore: the markers must survive admission,
        // because the parent-edge capture reads them after the seam and every
        // child launch scrubs its own command instead.
        let thread = std::env::var("CODEX_THREAD_ID");
        let claude = std::env::var("CLAUDE_CODE_SESSION_ID");
        admitted.expect("a plain claude spawn admits");
        assert_eq!(thread.as_deref(), Ok("01a0bba2-parent"));
        assert_eq!(claude.as_deref(), Ok("seed-2222-3333"));
    }
}
