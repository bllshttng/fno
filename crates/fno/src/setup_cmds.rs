//! The `fno config setup` commands, claimed before clap like `setup_run`:
//! `plan`, `post-merge`, the two `cli-hooks` commands (in `cli_hooks`), the
//! bare listing, and a refusal for an unknown name. `run` and `auto-wire`
//! belong to their own modules. Every root `fno setup <command>` spelling is
//! claimed too: the deprecated root group lives nowhere else now.
//!
//! `plan` prints the wizard question plan, rendered from the committed schema
//! table (`config_schema::wizard_plan`).
//!
//! `post-merge` asks for this repo's `post_merge.parking_lot_path` (and a
//! `project.id` when unset). It reads readiness from `fno config doctor
//! --post-merge --json` and writes through `fno config set --local`, so the
//! one validator decides every value. A rejected value (exit 2) asks again; a
//! structural refusal (exit 1) stops, because a new value cannot fix it.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

const PLAN_USAGE: &str = "usage: fno config setup plan [--advanced]";
const POST_MERGE_USAGE: &str = "usage: fno config setup post-merge";

/// The one key the scaffold must set. The Python registry owns its schema.
const PARKING_KEY: &str = "post_merge.parking_lot_path";

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Plan(Vec<OsString>),
    PostMerge(Vec<OsString>),
    CliHooks(Vec<OsString>),
    CliHooksCodex(Vec<OsString>),
    /// A bare `config setup`, or its `--help`.
    Help,
    /// A root `fno setup run|auto-wire`: those live under `config setup` only.
    Moved(String),
    Unknown(String),
}

/// The `config setup` commands, for the help listing.
const COMMANDS: &[(&str, &str)] = &[
    (
        "run",
        "Run setup once over the one step table (--yes for no prompts).",
    ),
    (
        "auto-wire",
        "Wire the fno plugin into every detected harness.",
    ),
    ("plan", "Emit the wizard question plan as JSON."),
    (
        "post-merge",
        "Scaffold post_merge.parking_lot_path for this repo.",
    ),
    (
        "cli-hooks",
        "Wire footnote's user-level CLI hooks (Codex, Gemini, Claude).",
    ),
    ("cli-hooks-codex", "Wire only the Codex SessionStart hook."),
];

/// Claim every `config setup` command except `run` and `auto-wire`, which
/// their own modules claim, and every root `fno setup` spelling.
pub fn classify(args: &[OsString]) -> Option<Cmd> {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return None;
    }
    let (at, root) = match words.as_slice() {
        ["config", "setup", ..] => (2, false),
        ["setup", ..] => (1, true),
        _ => return None,
    };
    // A bare `config setup` has no word at `at`, so slice only after a match.
    let rest = || args[at + 1..].to_vec();
    Some(match words.get(at).copied() {
        None | Some("-h" | "--help") => Cmd::Help,
        Some("plan") => Cmd::Plan(rest()),
        Some("post-merge") => Cmd::PostMerge(rest()),
        Some("cli-hooks") => Cmd::CliHooks(rest()),
        Some("cli-hooks-codex") => Cmd::CliHooksCodex(rest()),
        Some(w @ ("run" | "auto-wire")) if root => Cmd::Moved(w.to_string()),
        Some("run" | "auto-wire") => return None,
        Some(w) => Cmd::Unknown(w.to_string()),
    })
}

fn listing() -> String {
    let mut out = String::from("usage: fno config setup <command> [options]\n\ncommands:\n");
    for (name, what) in COMMANDS {
        out.push_str(&format!("  {name:<16} {what}\n"));
    }
    out
}

pub fn run(cmd: Cmd) -> i32 {
    match cmd {
        Cmd::Plan(rest) => run_plan(&rest),
        Cmd::PostMerge(rest) => run_post_merge(&rest),
        Cmd::CliHooks(rest) => crate::cli_hooks::run(&rest, false),
        Cmd::CliHooksCodex(rest) => crate::cli_hooks::run(&rest, true),
        Cmd::Help => {
            print!("{}", listing());
            0
        }
        Cmd::Moved(word) => {
            eprintln!("`fno setup {word}` moved: run `fno config setup {word}`");
            2
        }
        Cmd::Unknown(word) => {
            eprintln!("unknown setup command: '{word}'\n\n{}", listing());
            2
        }
    }
}

/// The plan's fields: the `always` tier only, or every asked field with
/// `advanced`.
pub fn plan_json(advanced: bool) -> String {
    crate::config_schema::wizard_plan(advanced)
}

fn run_plan(rest: &[OsString]) -> i32 {
    let mut advanced = false;
    for arg in rest {
        match arg.to_str() {
            Some("--advanced") => advanced = true,
            Some("-h" | "--help") => {
                println!("{PLAN_USAGE}\n\nEmit the wizard question plan as JSON. Without --advanced, only the 'always' tier.");
                return 0;
            }
            _ => {
                eprintln!("{PLAN_USAGE}");
                return 2;
            }
        }
    }
    println!("{}", plan_json(advanced));
    0
}

/// The scaffold's side effects, so a test drives the flow with no terminal
/// and no `fno` binary.
pub trait Io {
    /// The readiness verdict: the `fno config doctor --post-merge --json`
    /// object.
    fn readiness(&mut self) -> Result<Value, String>;
    /// Write one project-local key. `Err((exit code, message))` on refusal.
    fn set(&mut self, key: &str, value: &str) -> Result<(), (i32, String)>;
    /// Ask with an editable default. `None` is a cancel (EOF).
    fn prompt(&mut self, message: &str, default: &str) -> Option<String>;
    /// Ask yes or no, default no.
    fn confirm(&mut self, message: &str) -> bool;
    fn echo(&mut self, line: &str);
}

/// How the scaffold ended. Only `Failed` exits non-zero.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Written(String),
    AlreadyReady,
    Cancelled,
    Failed,
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The post-merge scaffold over `io`. `repo_name` seeds the suggested path
/// and the `project.id` default.
pub fn scaffold(io: &mut dyn Io, repo_name: &str) -> Outcome {
    let verdict = match io.readiness() {
        Ok(v) => v,
        Err(e) => {
            io.echo(&format!("cannot read post-merge readiness: {e}"));
            return Outcome::Failed;
        }
    };
    let project_id = field(&verdict, "project_id").map(str::to_string);
    match field(&verdict, "status") {
        Some("error") => {
            // A config file that does not parse fails every write too, so a
            // prompt loop here never ends. Stop with the real cause.
            let cause = field(&verdict, "cause").unwrap_or("unknown cause");
            io.echo(&format!(
                "the project config could not be read: {cause}. Fix it before scaffolding post-merge config."
            ));
            return Outcome::Failed;
        }
        Some("ready") => {
            let path = field(&verdict, "parking_lot_path").unwrap_or("");
            io.echo(&format!(
                "post-merge already configured: parking_lot_path={path}"
            ));
            if let Some(id) = &project_id {
                io.echo(&format!("project.id={id}"));
            }
            if !io.confirm("Update the existing parking_lot_path?") {
                return Outcome::AlreadyReady;
            }
        }
        _ => {}
    }

    let id = project_id.clone().unwrap_or_else(|| repo_name.to_string());
    let suggested = format!("internal/{id}/backlog/parking-lot.md");
    io.echo(
        "post_merge.parking_lot_path is repo-relative and is NOT derived from the project name. \
         The vault area often differs from the project (example-pipeline -> \
         internal/etl/backlog/parking-lot.md). The suggested value below is editable; \
         only what you confirm is written.",
    );

    let written = loop {
        let Some(value) = io.prompt(PARKING_KEY, &suggested) else {
            return Outcome::Cancelled;
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            return Outcome::Cancelled;
        }
        match io.set(PARKING_KEY, &value) {
            Ok(()) => break value,
            // An invalid value (an absolute or `..` path): ask again.
            Err((2, message)) => io.echo(&format!("rejected: {message}")),
            Err((_, message)) => {
                io.echo(&format!("cannot write config: {message}"));
                return Outcome::Failed;
            }
        }
    };

    // project.id is noted, never required: offer the repo name when unset.
    if project_id.is_none() {
        if let Some(id) = io.prompt("project.id (optional; for clean provenance)", repo_name) {
            let id = id.trim();
            if !id.is_empty() {
                if let Err((_, message)) = io.set("project.id", id) {
                    io.echo(&format!("project.id not set: {message}"));
                }
            }
        }
    }
    Outcome::Written(written)
}

/// The terminal and `fno` side of [`Io`].
struct Terminal {
    exe: PathBuf,
    root: PathBuf,
}

impl Terminal {
    /// Every child runs in `root` with `FNO_REPO_ROOT` pinned to it. The
    /// readiness read resolves the repo from git at its cwd, and `config set`
    /// resolves it from the env, so both must name the same repo.
    fn fno(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        std::process::Command::new(&self.exe)
            .args(args)
            .current_dir(&self.root)
            .env("FNO_REPO_ROOT", &self.root)
            .stdin(std::process::Stdio::null())
            .output()
    }
}

fn read_answer() -> Option<String> {
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
    }
}

impl Io for Terminal {
    fn readiness(&mut self) -> Result<Value, String> {
        let out = self
            .fno(&["config", "doctor", "--post-merge", "--json"])
            .map_err(|e| format!("fno config doctor: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "fno config doctor exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("fno config doctor: {e}"))
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), (i32, String)> {
        let out = self
            .fno(&["config", "set", key, value, "--local"])
            .map_err(|e| (1, format!("fno config set {key}: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err((out.status.code().unwrap_or(1), detail))
    }

    fn prompt(&mut self, message: &str, default: &str) -> Option<String> {
        print!("{message} [{default}]: ");
        read_answer().map(|a| {
            if a.trim().is_empty() {
                default.to_string()
            } else {
                a
            }
        })
    }

    fn confirm(&mut self, message: &str) -> bool {
        print!("{message} [y/N]: ");
        matches!(
            read_answer().as_deref().map(str::trim),
            Some("y" | "Y" | "yes")
        )
    }

    fn echo(&mut self, line: &str) {
        println!("{line}");
    }
}

fn run_post_merge(rest: &[OsString]) -> i32 {
    if let Some(arg) = rest.first() {
        if matches!(arg.to_str(), Some("-h" | "--help")) {
            println!("{POST_MERGE_USAGE}\n\nScaffold post_merge.parking_lot_path (+ project.id) for this repo.");
            return 0;
        }
        eprintln!("{POST_MERGE_USAGE}");
        return 2;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno"));
    let root = crate::setup_run::repo_root();
    let name = repo_name(&root);
    let mut term = Terminal { exe, root };
    match scaffold(&mut term, &name) {
        Outcome::Written(_) => {
            // The fresh verdict line, so the person sees the gate turn ready.
            if let Ok(out) = term.fno(&["config", "doctor", "--post-merge"]) {
                print!("{}", String::from_utf8_lossy(&out.stdout));
            }
            0
        }
        Outcome::AlreadyReady => 0,
        Outcome::Cancelled => {
            println!("setup post-merge: cancelled; nothing written.");
            0
        }
        Outcome::Failed => 1,
    }
}

fn repo_name(root: &Path) -> String {
    root.canonicalize()
        .ok()
        .as_deref()
        .unwrap_or(root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string())
}

#[cfg(test)]
#[path = "setup_cmds_tests.rs"]
mod tests;
