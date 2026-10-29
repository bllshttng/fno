//! `fno config setup run`: the agent host of setup, over the one step table.
//!
//! The single step list both surfaces ask: the terminal prompt path prints
//! `Install X? It will do Y. [Y/n]` per action step, and the chat skill asks
//! the same rows through `--list --json` and replays the accepted ids with
//! `--only`. The no-prompt path (`--yes`, a non-TTY stdin, or an agent env)
//! takes every recommended default and never reads stdin, so an agent can
//! never block on a prompt.
//!
//! `--dry-run` is the read-only preview of the same table: it prints one
//! JSON object through `redact` and writes nothing, no marker included. A
//! `--json` caller gets a refused object with `status: refused` on bad
//! argv, never prose on stderr.
//!
//! Layers: `global` once per machine (harness wiring, gh login, the
//! concurrency band), `project` once per repo, `contributor` only in a
//! footnote source checkout. Setup writes only keys whose value differs
//! from the default (`config_defaults::differs`). Steps that would edit a
//! shell rc or a provider account print their command instead (`report`
//! kind), and a browser login is `human` kind: under `--yes` those land in
//! the report, never acted silently. On success each layer writes a done
//! marker; `--once` refuses to redo a fully marked run.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

/// The step rows the chat skill asks through `--list --json` and the
/// terminal asks one by one. `act` steps may write config through
/// `fno config set`; `report` steps only ever print a command; `human`
/// steps need a person at a browser or terminal.
#[derive(Debug)]
pub struct Step {
    pub id: &'static str,
    pub layer: Layer,
    pub kind: Kind,
    pub question: &'static str,
    pub effect: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Global,
    Project,
    Contributor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Act,
    /// Proposes, never runs bare: under `--yes` the step lands in the
    /// report's `proposed` list with its exact command, and it executes
    /// only when `--only` names its id. An installer tail that passes
    /// `--yes` therefore never loads a service.
    Approve,
    Report,
    Human,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
    #[default]
    Both,
}

/// One proposed write: what a step would set if accepted. `key` empty
/// means a command proposal - the exact command rides in `reason`. Join
/// rows carry profile keys, so `key` is owned text, not a table literal.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: &'static str,
    pub key: String,
    pub value: String,
    pub scope: &'static str,
    pub reason: String,
    pub source: &'static str,
}

impl Proposal {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "id": self.id,
            "key": self.key,
            "value": self.value,
            "scope": self.scope,
            "reason": self.reason,
            "source": self.source,
        })
    }
}

/// Every row code and its fix line - the `next_action` the report names.
/// Append-only: a code lands here when its step lands, and [`row`] refuses
/// to build a code this table does not carry.
const CODES: &[(&str, &str)] = &[
    ("gh_not_signed_in", "gh auth login"),
    ("gh_scope_missing", "gh auth refresh -s repo -s workflow"),
    (
        "gh_unreadable",
        "install the GitHub CLI, then rerun `fno config setup run`",
    ),
    (
        "wiring_failed",
        "read the wiring line above, fix it, rerun `fno config setup run`",
    ),
    ("wiring_manual", "finish the named integration by hand"),
    (
        "config_set_refused",
        "run the `fno config set` line named in the message by hand",
    ),
    (
        "service_platform",
        "run the named command on a macOS machine",
    ),
    (
        "service_install",
        "read the error above, fix it, rerun `fno config setup run`",
    ),
    (
        "service_dependency",
        "load the named service first, then rerun `fno config setup run`",
    ),
    (
        "verify_red",
        "read the named check above, fix it, rerun `fno config setup run`",
    ),
    (
        "no_source",
        "answer the wizard prompt or put the keys in a profile, then rerun `fno config setup run`",
    ),
    (
        "join_backup_failed",
        "copy the global config file somewhere safe by hand, then rerun `fno config setup run --join <file>`",
    ),
    (
        "join_restored",
        "read the failing `fno config set` line above, fix it, rerun `fno config setup run --join <file>`",
    ),
    (
        "join_pending",
        "inspect the backup named in the message, restore the global config by hand if it is wrong, then remove the in-progress flag",
    ),
    (
        "projects_hand_set",
        "move the hand-set work.workspaces.main.projects value into an array of {name, path} tables, then rerun `fno config setup run`",
    ),
    (
        "messaging_unfinished",
        "read the messaging line above, then rerun `fno config setup run --only messaging`",
    ),
];

/// One problem a run or a dry-run hit: which list it sat on, a stable
/// code, the human message, and the fix that resolves it.
#[derive(Debug)]
pub struct Row {
    list: &'static str,
    code: &'static str,
    message: String,
    fix: &'static str,
}

impl Row {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "list": self.list,
            "code": self.code,
            "message": self.message,
            "fix": self.fix,
        })
    }
}

/// Build a row, pulling its fix from [`CODES`]. A code missing from the
/// table is a programming error, not a runtime state.
fn row(list: &'static str, code: &'static str, message: String) -> Row {
    let fix = CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, f)| *f)
        .expect("row code missing from CODES");
    Row {
        list,
        code,
        message,
        fix,
    }
}

/// One run's outcome entries. The JSON report carries exactly these
/// fourteen fields, so an agent reads the same facts the human lines
/// print; `proposed`, `verified`, `rows` and `next_action` carry the
/// dry-run contract.
pub struct Report {
    done: Vec<String>,
    skipped: Vec<String>,
    needs_human: Vec<String>,
    paths: Vec<String>,
    restart_needed: bool,
    undo: Option<String>,
    proposed: Vec<Proposal>,
    verified: Vec<String>,
    rows: Vec<Row>,
    next_action: String,
    schema: u8,
    run_id: String,
    host: &'static str,
    status: &'static str,
}

impl Default for Report {
    fn default() -> Self {
        Report {
            done: Vec::new(),
            skipped: Vec::new(),
            needs_human: Vec::new(),
            paths: Vec::new(),
            restart_needed: false,
            undo: None,
            proposed: Vec::new(),
            verified: Vec::new(),
            rows: Vec::new(),
            next_action: String::new(),
            schema: 1,
            run_id: String::new(),
            host: "agent",
            status: "finished",
        }
    }
}

impl Report {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "run_id": self.run_id,
            "host": self.host,
            "status": self.status,
            "done": self.done,
            "skipped": self.skipped,
            "needs_human": self.needs_human,
            "paths": self.paths,
            "restart_needed": self.restart_needed,
            "undo": self.undo,
            "proposed": self.proposed.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
            "verified": self.verified,
            "rows": self.rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
            "next_action": self.next_action,
        })
    }
}

/// A short unique id for one run or refusal: nanos plus the pid's low
/// bits, hex. Not a security token - a correlate for logs and mails.
pub(crate) fn run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{:04x}", std::process::id() & 0xffff)
}

/// The first red row's fix when one exists, else the fallback (the
/// /fno:target line on a healthy machine).
fn next_action(rows: &[Row], fallback: &str) -> String {
    rows.iter()
        .find(|r| r.list == "needs_human" && !r.fix.is_empty())
        .map(|r| r.fix.to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// The one JSON object a `--json` caller reads on bad argv, in place of
/// the prose refusal. Same envelope, `status: refused`.
fn refused_json(message: &str) -> serde_json::Value {
    json!({
        "schema": 1,
        "run_id": run_id(),
        "host": "agent",
        "status": "refused",
        "message": message,
        "next_action": message,
    })
}

/// The step table. Order is the order both surfaces ask in.
pub const STEPS: &[Step] = &[
    Step {
        id: "harness-wiring",
        layer: Layer::Global,
        kind: Kind::Act,
        question: "Wire the fno plugin into every agent CLI on PATH?",
        effect: "installs the fno plugin into each detected agent CLI (claude, codex, gemini, opencode, pi, agy)",
    },
    Step {
        id: "gh-auth",
        layer: Layer::Global,
        kind: Kind::Human,
        question: "Sign in to the GitHub CLI?",
        effect: "run `gh auth login`; without it fno stays local-only (no PR, no review, no merge)",
    },
    Step {
        id: "concurrency-band",
        layer: Layer::Global,
        kind: Kind::Act,
        question: "Write this machine's measured concurrency band as agents.max_live?",
        effect: "measures the machine and writes agents.max_live only when it differs from the default",
    },
    Step {
        id: "accounts",
        layer: Layer::Global,
        kind: Kind::Report,
        question: "Provider accounts and rotation?",
        effect: "review later with `fno config accounts`; login needs a browser, so setup only names the command",
    },
    Step {
        id: "guardrails",
        layer: Layer::Global,
        kind: Kind::Act,
        question: "Keep the standard guardrail preset?",
        effect: "writes guards.preset = standard only when the file carries a different value",
    },
    Step {
        id: "pr-watcher",
        layer: Layer::Global,
        kind: Kind::Approve,
        question: "Install and load the pr-watcher launchd agent?",
        effect: "runs `fno-agents pr-watch install --ensure` and writes pr_watch.enabled = true",
    },
    Step {
        id: "auto-heal",
        layer: Layer::Global,
        kind: Kind::Approve,
        question: "Turn on the auto-heal loop?",
        effect: "writes auto_heal.enabled = true; needs the pr-watcher agent loaded",
    },
    Step {
        id: "groom-agent",
        layer: Layer::Global,
        kind: Kind::Approve,
        question: "Install the daily groom launchd agent?",
        effect: "runs `fno backlog groom --install-agent`",
    },
    Step {
        id: "projects",
        layer: Layer::Global,
        kind: Kind::Approve,
        question: "Add this repo to the main workspace list?",
        effect: "appends {name, path} rows for this repo to the workspace list",
    },
    Step {
        id: "review-merge",
        layer: Layer::Global,
        kind: Kind::Approve,
        question: "Set the review posture and auto-merge grant?",
        effect: "writes review.posture, auto_merge.enabled and auto_merge.grant from the wizard answer or a profile, never a default",
    },
    Step {
        id: "messaging",
        layer: Layer::Global,
        kind: Kind::Human,
        question: "Set up a chat channel for questions?",
        effect: "pick BlueBubbles (iMessage), Telegram or Discord; each open question posts there and a reply in its thread answers it",
    },
    Step {
        id: "machine-tools",
        layer: Layer::Global,
        kind: Kind::Report,
        question: "Check the optional machine tools?",
        effect: "names `brew install macmon` when the resource meter's sampler is absent",
    },
    Step {
        id: "permission-rules",
        layer: Layer::Global,
        kind: Kind::Report,
        question: "Show the narrow allow rules to add by hand?",
        effect: "prints the Bash(fno:*) permissions.allow example; never edits a harness settings file",
    },
    Step {
        id: "worktree-policy",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Pin a worktree policy for this repo?",
        effect: "writes worktree.policy; the recommended default is unset (harness-native behavior)",
    },
    Step {
        id: "plans-dir",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Keep .fno/plans/ as this repo's plans dir?",
        effect: "writes plans_dir only when this repo should keep plans somewhere else",
    },
    Step {
        id: "review-bots",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Name external review bots for this repo?",
        effect: "writes review.github_apps only when this repo wants bot reviews",
    },
    Step {
        id: "backlog-prefix",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Mint backlog node IDs as fn-<4 hex> for this repo?",
        effect: "writes backlog.id_prefix = fn and backlog.id_hex_width = 4 together when no prefix is configured",
    },
    Step {
        id: "cargo-toolchain",
        layer: Layer::Contributor,
        kind: Kind::Report,
        question: "Rust toolchain?",
        effect: "contributors need rustup's stable toolchain: `curl https://sh.rustup.rs -sSf | sh`",
    },
    Step {
        id: "crates-build",
        layer: Layer::Contributor,
        kind: Kind::Report,
        question: "Build the Rust crates?",
        effect: "run `cargo build --locked` at the checkout root; a release install never needs this",
    },
    Step {
        id: "cargo-bin-on-path",
        layer: Layer::Contributor,
        kind: Kind::Report,
        question: "Put ~/.cargo/bin on PATH?",
        effect: "add ~/.cargo/bin to PATH so the built binaries resolve; setup names the line, never edits the rc",
    },
    Step {
        id: "join",
        layer: Layer::Global,
        kind: Kind::Approve,
        question: "Apply the exported profile to this machine's global config?",
        effect: "backs up the global config, then applies the profile's portable keys",
    },
];

/// This command's argv, claimed lexically before clap: exactly
/// `config setup run` plus the flags `parse_opts` accepts. The sibling
/// `auto-wire` and `plan` spellings stay with their owners.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return None;
    }
    match words.as_slice() {
        ["config", "setup", "run", ..] => Some(args[3..].to_vec()),
        _ => None,
    }
}

/// `--yes` / `--once` / `--scope` / `--only` / `--list` / `--json` /
/// `--dry-run` / `--scan-repos` / `--scan-dirs` / `--export` / `--out` /
/// `--join`, hand parsed: the command is claimed before clap, so the flags
/// are ours. An unknown flag or value refuses with the usage line and
/// exit 2.
pub fn parse_opts(tail: &[OsString]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < tail.len() {
        let flag = tail[i].to_str().unwrap_or("");
        match flag {
            "--help" | "-h" => o.help = true,
            "--yes" => o.yes = true,
            "--once" => o.once = true,
            "--list" => o.list = true,
            "--json" | "-J" => o.json = true,
            "--dry-run" | "-N" => o.dry_run = true,
            "--scan-repos" => o.scan = true,
            "--scan-dirs" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--scan-dirs needs a comma list of directories")?;
                o.scan_dirs = v.split(',').map(|s| s.trim().to_string()).collect();
                o.scan = true;
            }
            "--export" => o.export = true,
            "--out" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--out needs a file path")?;
                o.out = Some(v.to_string());
            }
            "--join" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--join needs a profile file or - for stdin")?;
                o.join = Some(v.to_string());
            }
            "--scope" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--scope needs global|project|both")?;
                o.scope = match v {
                    "global" => Scope::Global,
                    "project" => Scope::Project,
                    "both" => Scope::Both,
                    other => return Err(format!("--scope: unknown value {other}")),
                };
            }
            "--only" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--only needs a comma list of step ids")?;
                o.only = v.split(',').map(|s| s.trim().to_string()).collect();
            }
            "--check" => o.check = true,
            _ => return Err(format!("unknown flag {flag}; {USAGE}")),
        }
        i += 1;
    }
    Ok(o)
}

/// The no-prompt decision, factored so a test passes each input: an
/// explicit `--yes`, a non-TTY stdin, or an agent session env takes the
/// recommended default for every step and never reads stdin.
fn no_prompt(yes: bool, stdin_is_tty: bool, agent_env: bool) -> bool {
    yes || !stdin_is_tty || agent_env
}

/// `CLAUDECODE` / `CLAUDE_CODE_CHILD_SESSION`: either names an agent
/// session, where a stdout prompt would hang the run forever.
fn agent_env() -> bool {
    ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION"]
        .iter()
        .any(|k| std::env::var_os(k).map(|v| !v.is_empty()).unwrap_or(false))
}

/// UTC now as `YYYY-MM-DDTHH:MM:SSZ`, no external time dep: days-from-civil
/// inverted (Hinnant), exact for any date a marker will carry.
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// This run's flags.
#[derive(Default)]
pub struct Opts {
    yes: bool,
    once: bool,
    scope: Scope,
    only: Vec<String>,
    list: bool,
    json: bool,
    dry_run: bool,
    check: bool,
    help: bool,
    scan: bool,
    scan_dirs: Vec<String>,
    export: bool,
    out: Option<String>,
    join: Option<String>,
}

/// The argv tail `parse_opts` accepts, repeated for refusals.
const USAGE: &str = "usage: fno config setup run [--help] [--yes] [--once] [--scope global|project|both] [--only <id,...>] [--list] [--json] [--dry-run] [--check] [--scan-repos] [--scan-dirs <dir,...>] [--export] [--out <file>] [--join <file or ->]";

/// Every non-usage refusal exit: the same code the Python CLI uses for a
/// bad flag or value.
const EXIT_USAGE: i32 = 2;

/// The global layer's done marker: inside the state root's `sidecar/`
/// subfolder, the one owned per-item area the inventory already names, so
/// the root itself grows no new row.
fn global_marker() -> PathBuf {
    crate::model_catalog::state_dir()
        .join("sidecar")
        .join("setup-done")
}

/// The project layer's done marker, beside the repo's `.fno/config.toml`.
fn project_marker(root: &Path) -> PathBuf {
    root.join(".fno").join("setup.done")
}

/// Marker content: the fno version and the UTC stamp, so a reader can tell
/// a stale marker from a live one.
fn marker_line() -> String {
    format!("fno {} {}", env!("CARGO_PKG_VERSION"), utc_now())
}

/// The nearest repo root: `$FNO_REPO_ROOT` when pinned, else the nearest
/// ancestor holding a `.git` dir, else the working directory. The same
/// resolution `config_defaults` applies to the project config file.
pub(crate) fn repo_root() -> PathBuf {
    if let Some(root) = std::env::var_os("FNO_REPO_ROOT").filter(|r| !r.is_empty()) {
        return PathBuf::from(root);
    }
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if cwd.join(".git").exists() {
            return cwd;
        }
        if !cwd.pop() {
            return PathBuf::from(".");
        }
    }
}

/// A footnote source checkout: the fno crate manifest under `crates/`.
/// Contributors get the three report steps; a release install never sees
/// them. One joined path, not chained `join("fno")` literals: the
/// seam-crossings ratchet reads that literal shape as a porcelain resolver.
fn in_source_checkout(cwd: &Path) -> bool {
    cwd.join("crates/fno/Cargo.toml").is_file()
}

/// Write one or more `key=value` pairs through `fno config set` (the
/// validated writer: coercion, schema, reserved families) as ONE call, so
/// a multi-key pair lands atomically: any refused value leaves the file
/// unchanged. Global scope by default, `--local` for the project layer.
/// Returns the config file written, so the report's paths stay absolute.
fn config_set(exe: &Path, pairs: &[(&str, &str)], local: bool) -> Result<PathBuf, String> {
    config_set_split(exe, pairs, local).map_err(|(message, _refused)| message)
}

/// [`config_set`] split by failure shape, which only the join step cares
/// about: a refusal is the writer's validator saying no (non-zero exit,
/// the file untouched, the remaining keys can still land), while a spawn
/// or io failure may have half-landed, so the caller must restore from
/// its backup. The bool is `true` exactly when the error is a refusal.
fn config_set_split(
    exe: &Path,
    pairs: &[(&str, &str)],
    local: bool,
) -> Result<PathBuf, (String, bool)> {
    let mut argv: Vec<OsString> = vec![exe.to_path_buf().into(), "config".into(), "set".into()];
    for (key, value) in pairs {
        argv.push(format!("{key}={value}").into());
    }
    if local {
        argv.push("--local".into());
    }
    let keys = pairs.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(" ");
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = cmd
        .spawn()
        .and_then(|c| c.wait_with_output())
        .map_err(|e| (format!("fno config set {keys}: {e}"), false))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err((
            format!(
                "`fno config set {keys}` exited {}: {detail}",
                out.status.code().unwrap_or(-1)
            ),
            true,
        ));
    }
    if local {
        Ok(repo_root().join(".fno").join("config.toml"))
    } else {
        Ok(crate::model_catalog::state_dir().join("config.toml"))
    }
}

/// Fold one auto-wire outcome line into the report. The command's lines are
/// stable human text pinned by its own tests: `Label: installed`,
/// `Label: already installed`, `needs a manual finish`, `FAILED`.
fn fold_wire_line(line: &str, rep: &mut Report) {
    let t = line.trim();
    if t.is_empty() || t.starts_with("wired the fno plugin") {
        return;
    }
    if t.contains("FAILED") || t.contains("needs a manual finish") {
        let message = format!("harness-wiring: {t}");
        let code = if t.contains("FAILED") {
            "wiring_failed"
        } else {
            "wiring_manual"
        };
        rep.rows.push(row("needs_human", code, message.clone()));
        rep.needs_human.push(message);
    } else if t.contains(": installed") || t.contains(": already installed") {
        rep.done.push(format!("harness-wiring: {t}"));
        // A fresh install asks for a restart; an "already installed" line
        // changed nothing, so it must not.
        if !t.contains("already") {
            rep.restart_needed = true;
        }
    } else {
        rep.skipped.push(format!("harness-wiring: {t}"));
    }
}

/// The gh login probe: authenticated is skipped, otherwise the step is the
/// report's one needs-human row, because login opens a browser.
fn step_gh_auth(rep: &mut Report) {
    let out = std::process::Command::new("gh")
        .arg("auth")
        .arg("status")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            rep.skipped
                .push("gh-auth: gh is already authenticated".into());
        }
        Ok(o) => {
            let detail = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let message = format!(
                "gh-auth: run `gh auth login` - login opens a browser; without it fno stays local-only (no PR, no review, no merge){}",
                if detail.is_empty() { String::new() } else { format!(" ({detail})") }
            );
            rep.rows
                .push(row("needs_human", "gh_not_signed_in", message.clone()));
            rep.needs_human.push(message);
        }
        Err(e) => {
            let message = format!(
                "gh-auth: run `gh auth login` - login opens a browser; gh itself is missing ({e})"
            );
            rep.rows
                .push(row("needs_human", "gh_unreadable", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The proposal half of the harness-wiring step: what the auto-wire command
/// would do, without running it. The run path runs the command; `--dry-run`
/// prints this row. Empty `key` marks a command proposal.
fn propose_harness_wiring() -> Proposal {
    Proposal {
        id: "harness-wiring",
        key: "".into(),
        value: String::new(),
        scope: "global",
        reason:
            "fno config setup auto-wire installs the fno plugin into every agent CLI found on PATH"
                .to_string(),
        source: "default",
    }
}

/// The skip line a no-sample machine reads: the daemon files the band
/// question on its own schedule, so setup says so instead of guessing.
const NO_BAND_SAMPLE: &str =
    "concurrency-band: no machine sample yet; the daemon files the band question within five minutes";

/// The proposal half of the concurrency-band step, pure of writes: the
/// band number from the machine sample's clause via the one shared
/// parser, or `None` with the skip reason pushed. The run path and
/// `--dry-run` both call it, so the proposal cannot drift from the write.
fn propose_concurrency_band(skips: &mut Vec<String>) -> Option<i64> {
    let band = crate::setup_detect::budget_clause()
        .and_then(|t| crate::setup_detect::parse_budget_clause(&t));
    match band {
        None => {
            skips.push(NO_BAND_SAMPLE.to_string());
            None
        }
        Some(n) => {
            let value = toml::Value::Integer(n);
            if !crate::config_defaults::differs("agents.max_live", &value) {
                skips.push(format!("concurrency-band: agents.max_live already {n}"));
                return None;
            }
            Some(n)
        }
    }
}

/// The concurrency band: the band number the machine sample printed,
/// written only when it differs from the configured value.
fn step_concurrency_band(exe: &Path, rep: &mut Report) {
    let mut skips = Vec::new();
    let Some(n) = propose_concurrency_band(&mut skips) else {
        rep.skipped.extend(skips);
        return;
    };
    match config_set(exe, &[("agents.max_live", &n.to_string())], false) {
        Ok(path) => {
            rep.done
                .push(format!("concurrency-band: agents.max_live = {n}"));
            rep.paths.push(path.display().to_string());
            rep.restart_needed = true;
        }
        Err(e) => {
            let message = format!("concurrency-band: {e}");
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The proposal half of the guardrails step: the recommended preset when
/// the global file carries a different value, else `None` with the skip
/// reason pushed. Shared by the run path and `--dry-run`.
fn propose_guardrails(skips: &mut Vec<String>) -> Option<Proposal> {
    let value = toml::Value::String("standard".into());
    if !crate::config_defaults::differs("guards.preset", &value) {
        skips.push("guardrails: guards.preset already standard".into());
        return None;
    }
    Some(Proposal {
        id: "guardrails",
        key: "guards.preset".into(),
        value: "standard".to_string(),
        scope: "global",
        reason: "the standard guard preset turns on the refusal gates a fresh machine wants"
            .to_string(),
        source: "default",
    })
}

/// The guardrail preset step: recommended `standard`, written only when the
/// global file carries a different value.
fn step_guardrails(exe: &Path, rep: &mut Report) {
    let mut skips = Vec::new();
    let Some(p) = propose_guardrails(&mut skips) else {
        rep.skipped.extend(skips);
        return;
    };
    match config_set(exe, &[(p.key.as_str(), p.value.as_str())], false) {
        Ok(path) => {
            rep.done
                .push(format!("guardrails: {} = {}", p.key, p.value));
            rep.paths.push(path.display().to_string());
        }
        Err(e) => {
            let message = format!("guardrails: {e}");
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The proposal half of the backlog-prefix step: the fixed scheme (the
/// ruling: prefix `fn`, 4 hex digits, every repo) when no config file sets
/// the key yet, else `None` with the skip reason pushed. A configured
/// prefix is the project's own choice; setup never overwrites one.
fn propose_backlog_prefix(root: &Path, skips: &mut Vec<String>) -> Option<Proposal> {
    let global = crate::model_catalog::state_dir().join("config.toml");
    let project = root.join(".fno").join("config.toml");
    if crate::config_defaults::key_set(Some(&global), Some(&project), "backlog.id_prefix") {
        skips.push(
            "backlog-prefix: backlog.id_prefix is configured; setup never overwrites it".into(),
        );
        return None;
    }
    Some(Proposal {
        id: "backlog-prefix",
        key: "backlog.id_prefix".into(),
        value: "fn".into(),
        scope: "project",
        reason: "the fixed scheme: nodes mint as fn-<4 hex>; the width rides the same write".into(),
        source: "default",
    })
}

/// Whether a config file carries any `backlog.` key, i.e. a `[backlog]`
/// table. An unreadable file counts as having one, the `key_set` rule:
/// setup never writes a config it cannot read.
fn backlog_table_exists(path: &Path) -> bool {
    match crate::config_defaults::read_flat(path) {
        Ok(Some(map)) => map.keys().any(|k| k.starts_with("backlog.")),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// The backlog-prefix step: the pair written so it is never half set, one
/// `config set` call per layer, each carrying both keys as `key=value`
/// tokens. Project-scoped always; the global file too when it has no
/// `[backlog]` table, so a session in a fresh worktree (its project file
/// is untracked and absent there) still mints the scheme. A configured
/// prefix in either file is the project's own choice; setup never
/// overwrites it.
fn step_backlog_prefix(exe: &Path, root: &Path, rep: &mut Report) {
    let mut skips = Vec::new();
    let Some(p) = propose_backlog_prefix(root, &mut skips) else {
        rep.skipped.extend(skips);
        return;
    };
    let pairs = [
        ("backlog.id_prefix", p.value.as_str()),
        ("backlog.id_hex_width", "4"),
    ];
    let global = crate::model_catalog::state_dir().join("config.toml");
    let mut targets: Vec<(bool, &str)> = vec![(true, "project")];
    if !backlog_table_exists(&global) {
        targets.push((false, "global"));
    }
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for (local, label) in targets {
        match config_set(exe, &pairs, local) {
            Ok(path) => paths.push(path),
            Err(e) => errors.push(format!("{label}: {e}")),
        }
    }
    if !paths.is_empty() {
        rep.done.push(format!(
            "backlog-prefix: backlog.id_prefix = {}, backlog.id_hex_width = 4",
            p.value
        ));
        rep.paths
            .extend(paths.into_iter().map(|p| p.display().to_string()));
    }
    for e in errors {
        let message = format!("backlog-prefix: {e}");
        rep.rows
            .push(row("needs_human", "config_set_refused", message.clone()));
        rep.needs_human.push(message);
    }
}

/// The join step: back up the global config, stamp the in-progress flag,
/// then apply the profile's portable keys one `fno config set` call at a
/// time, in diff order. A refused key is reported and the rest continue
/// (a refusal means the validator rejected the write and the file is
/// untouched); a failed call restores the backup and ends the run, and a
/// failed restore keeps the flag so the next start heals. Success removes
/// the flag, saves the profile, and stamps the report's one-line undo.
fn step_join(
    exe: &Path,
    profile: &crate::setup_profile::Profile,
    rows: &[crate::setup_profile::DiffRow],
    rep: &mut Report,
) {
    let apply: Vec<&crate::setup_profile::DiffRow> = rows.iter().filter(|r| r.applies()).collect();
    if apply.is_empty() {
        rep.skipped
            .push("join: nothing to apply; every key is present or machine-local".into());
        crate::setup_profile::save_profile(&profile.text);
        rep.paths
            .push(crate::setup_profile::profile_path().display().to_string());
        return;
    }
    let global = crate::config_defaults::global_config_path();
    let backup = match crate::setup_profile::backup(&global) {
        Ok(b) => b,
        Err(e) => {
            let message =
                format!("join: the global config could not be backed up ({e}); nothing applied");
            rep.rows
                .push(row("needs_human", "join_backup_failed", message.clone()));
            rep.needs_human.push(message);
            return;
        }
    };
    if let Err(e) = crate::setup_profile::mark_in_progress() {
        let message = format!(
            "join: the backup is taken but the in-progress flag could not be written ({e}); nothing applied"
        );
        rep.rows
            .push(row("needs_human", "join_backup_failed", message.clone()));
        rep.needs_human.push(message);
        return;
    }
    let mut applied = 0usize;
    for r in &apply {
        let value =
            crate::setup_profile::toml_inline(r.apply.as_ref().expect("apply rows carry a value"));
        match config_set_split(exe, &[(r.key.as_str(), value.as_str())], false) {
            Ok(path) => {
                applied += 1;
                rep.done.push(format!("join: {} = {value}", r.key));
                rep.paths.push(path.display().to_string());
            }
            Err((message, true)) => {
                let message = format!("join: {message}");
                rep.rows
                    .push(row("needs_human", "config_set_refused", message.clone()));
                rep.needs_human.push(message);
            }
            Err((message, false)) => {
                match crate::setup_profile::restore(&global, &backup) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(crate::setup_profile::in_progress_flag());
                        let message = format!(
                            "join: {message}; the global config was restored from its backup"
                        );
                        rep.rows
                            .push(row("needs_human", "join_restored", message.clone()));
                        rep.needs_human.push(message);
                    }
                    Err(e) => {
                        let message = format!(
                            "join: {message}; the restore also failed ({e}); the in-progress flag is kept so the next start heals"
                        );
                        rep.rows
                            .push(row("needs_human", "join_pending", message.clone()));
                        rep.needs_human.push(message);
                    }
                }
                // The config just went back to its original bytes: nothing
                // later may apply on top of the restored state.
                return;
            }
        }
    }
    if applied == 0 {
        // Every key refused, but the join still consumed its source:
        // persist the profile so a retry starts from here, matching the
        // empty branch below.
        let _ = std::fs::remove_file(crate::setup_profile::in_progress_flag());
        crate::setup_profile::save_profile(&profile.text);
        rep.paths
            .push(crate::setup_profile::profile_path().display().to_string());
        return;
    }
    let _ = std::fs::remove_file(crate::setup_profile::in_progress_flag());
    crate::setup_profile::save_profile(&profile.text);
    rep.paths
        .push(crate::setup_profile::profile_path().display().to_string());
    if let Some(b) = &backup {
        rep.undo = Some(format!("undo: cp {} {}", b.display(), global.display()));
    }
    rep.restart_needed = true;
    rep.done
        .push(format!("join: {applied} keys applied from the profile"));
}

/// The shared fold for a service step's config key: `config_set` is the
/// only writer, so both the Ok and Err shapes are one call each.
fn service_key_set(exe: &Path, step: &str, key: &str, value: &str, rep: &mut Report) {
    match config_set(exe, &[(key, value)], false) {
        Ok(path) => {
            rep.done.push(format!("{step}: {key} = {value}"));
            rep.paths.push(path.display().to_string());
        }
        Err(e) => {
            let message = format!("{step}: {e}");
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The non-macOS shape for a launchd step: a needs_human row carrying the
/// command, so the person runs it where launchd exists.
fn service_platform(step: &str, command: &str, rep: &mut Report) {
    let message = format!("{step}: launchd services are macOS-only; run `{command}` there");
    rep.rows
        .push(row("needs_human", "service_platform", message.clone()));
    rep.needs_human.push(message);
}

/// The pr-watcher: install and load the launchd agent, then write its
/// enable key. An already-loaded agent restarts nothing but still writes
/// the key, so `--only pr-watcher` leaves `pr_watch.enabled = true` on
/// disk whether or not the install ran this time.
fn step_pr_watcher(exe: &Path, rep: &mut Report, loaded: &mut Vec<String>) {
    const COMMAND: &str = "fno-agents pr-watch install --ensure";
    if !crate::setup_services::macos() {
        service_platform("pr-watcher", COMMAND, rep);
        return;
    }
    if crate::setup_services::is_loaded(crate::setup_services::PR_WATCHER_LABEL) {
        rep.skipped
            .push("pr-watcher: agent already loaded, restarting nothing".into());
        service_key_set(exe, "pr-watcher", "pr_watch.enabled", "true", rep);
        return;
    }
    let bin = crate::digest_overlay::fno_agents_bin();
    let Some(bin) = bin.to_str() else {
        let message = "pr-watcher: the fno-agents binary path is not valid UTF-8".to_string();
        rep.rows
            .push(row("needs_human", "service_install", message.clone()));
        rep.needs_human.push(message);
        return;
    };
    match crate::setup_detect::bounded_for(
        &[bin, "pr-watch", "install", "--ensure"],
        crate::setup_services::INSTALL_CAP,
    ) {
        Ok(_) => {
            rep.done
                .push("pr-watcher: agent installed and loaded".into());
            loaded.push(crate::setup_services::PR_WATCHER_LABEL.into());
            service_key_set(exe, "pr-watcher", "pr_watch.enabled", "true", rep);
        }
        Err(e) => {
            let message = format!("pr-watcher: {e}");
            rep.rows
                .push(row("needs_human", "service_install", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// Auto-heal rides the pr-watcher's loop: the loaded agent is the
/// dependency, the key is the consent. With no agent the key stays
/// unwritten - enabling heal where nothing watches would describe a state
/// nothing maintains.
fn step_auto_heal(exe: &Path, rep: &mut Report) {
    if !crate::setup_services::macos() {
        service_platform(
            "auto-heal",
            "fno-agents pr-watch install --ensure, then rerun with --only auto-heal",
            rep,
        );
        return;
    }
    if !crate::setup_services::is_loaded(crate::setup_services::PR_WATCHER_LABEL) {
        let message = "auto-heal: the pr-watcher agent is not loaded, install it first".to_string();
        rep.rows
            .push(row("needs_human", "service_dependency", message.clone()));
        rep.needs_human.push(message);
        return;
    }
    service_key_set(exe, "auto-heal", "auto_heal.enabled", "true", rep);
}

/// The groom agent: one launchd install, no config key behind it. A
/// loaded agent skips for the same reason the pr-watcher's does.
fn step_groom_agent(exe: &Path, rep: &mut Report, loaded: &mut Vec<String>) {
    if !crate::setup_services::macos() {
        service_platform("groom-agent", "fno backlog groom --install-agent", rep);
        return;
    }
    if crate::setup_services::is_loaded(crate::setup_services::GROOM_LABEL) {
        rep.skipped
            .push("groom-agent: agent already loaded, restarting nothing".into());
        return;
    }
    let exe_str = exe.to_string_lossy().into_owned();
    match crate::setup_detect::bounded_for(
        &[exe_str.as_str(), "backlog", "groom", "--install-agent"],
        crate::setup_services::INSTALL_CAP,
    ) {
        Ok(_) => {
            rep.done
                .push("groom-agent: agent installed and loaded".into());
            loaded.push(crate::setup_services::GROOM_LABEL.into());
        }
        Err(e) => {
            let message = format!("groom-agent: {e}");
            rep.rows
                .push(row("needs_human", "service_install", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// A TOML value as JSON for the workspace rows: scalars pass through, a
/// datetime reads as its string, arrays and tables recurse. Rows carry
/// only plain shapes, so this is the whole conversion. `pub(crate)`: the
/// profile diff converts TOML values through it for `classify`.
pub(crate) fn toml_as_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => json!(i),
        toml::Value::Float(f) => json!(f),
        toml::Value::Boolean(b) => json!(b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_as_json).collect()),
        toml::Value::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_as_json(v)))
                .collect(),
        ),
    }
}

/// The workspace list: this repo appended as one {name, path} row through
/// the validated writer, existing rows carried verbatim. A path already
/// listed skips - the list is the person's registry, setup only grows it.
fn step_projects(exe: &Path, root: &Path, rep: &mut Report) {
    const PROJECTS_KEY: &str = "work.workspaces.main.projects";
    let global = crate::config_defaults::global_config_path();
    let mut rows: Vec<serde_json::Value> = match crate::config_defaults::read_flat(&global) {
        Err(e) => {
            let message = format!("projects: cannot read {}: {e}", global.display());
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
            return;
        }
        Ok(None) => Vec::new(),
        Ok(Some(map)) => match projects_rows(map.get(PROJECTS_KEY)) {
            Ok(existing) => existing,
            Err(()) => {
                let message = format!(
                    "projects: {PROJECTS_KEY} is hand-set to a non-array value; setup would overwrite it"
                );
                rep.rows
                    .push(row("needs_human", "projects_hand_set", message.clone()));
                rep.needs_human.push(message);
                return;
            }
        },
    };
    let display = root.display().to_string();
    if rows
        .iter()
        .any(|r| r.get("path").and_then(serde_json::Value::as_str) == Some(display.as_str()))
    {
        rep.skipped
            .push("projects: this repo is already on the workspace list".into());
        return;
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| display.clone());
    rows.push(json!({ "name": name, "path": display }));
    let value = serde_json::to_string(&rows).unwrap_or_default();
    match config_set(exe, &[(PROJECTS_KEY, value.as_str())], false) {
        Ok(path) => {
            rep.done
                .push(format!("projects: added this repo to {PROJECTS_KEY}"));
            rep.paths.push(path.display().to_string());
        }
        Err(e) => {
            let message = format!("projects: {e}");
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The existing workspace list as JSON rows. A missing key and a real
/// array both pass through; any other hand-set shape refuses so the
/// caller never overwrites it with setup's own array.
fn projects_rows(value: Option<&toml::Value>) -> Result<Vec<serde_json::Value>, ()> {
    match value {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(items)) => Ok(items.iter().map(toml_as_json).collect()),
        Some(_) => Err(()),
    }
}

/// The review posture: no wizard answer and no profile exists on the run
/// host yet, so the keys wait for a source. Naming a default here would
/// grant auto-merge silently - the one write this step must never make.
fn step_review_merge(rep: &mut Report) {
    let message =
        "review-merge: no source for the posture yet (wizard answer or profile)".to_string();
    rep.rows
        .push(row("needs_human", "no_source", message.clone()));
    rep.needs_human.push(message);
}

/// The optional sampler the resource meter reads. Absent is a normal
/// state, so this only ever reports.
fn step_machine_tools(rep: &mut Report) {
    if crate::setup_autowire::on_path("macmon") {
        rep.skipped.push("machine-tools: macmon is on PATH".into());
    } else {
        rep.skipped
            .push("machine-tools: `brew install macmon` feeds the resource meter".into());
    }
}

/// The narrow harness allow rules, printed never written: a settings file
/// belongs to its harness and its person.
fn step_permission_rules(rep: &mut Report) {
    rep.skipped.push(
        "permission-rules: add \"Bash(fno:*)\" to permissions.allow in your harness settings by hand"
            .into(),
    );
}

/// Why a verify read failed, in one short clause: the timeout as
/// bounded_for words it, else the first line naming a problem and the line
/// after it (a doctor prints its header lines first), else the command to
/// re-run, else the caller's own reason.
fn verify_reason(name: &str, detail: &str) -> String {
    if detail.contains("timed out after") {
        return detail.rsplit(": ").next().unwrap_or(detail).to_string();
    }
    let lines: Vec<&str> = detail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let hit = lines.iter().position(|l| {
        let l = l.to_ascii_lowercase();
        ["blocker", "issue", "error", "fail", "missing"]
            .iter()
            .any(|w| l.contains(w))
    });
    match hit {
        Some(i) => lines[i..lines.len().min(i + 2)]
            .iter()
            .map(|l| l.chars().take(200).collect::<String>())
            .collect::<Vec<_>>()
            .join(" "),
        // bounded_for words a command's exit with a leading backtick; any
        // other detail is a reason the caller already wrote.
        None if detail.starts_with('`') => format!("run `{name}` to read why"),
        None => detail.trim().to_string(),
    }
}

/// The end-of-run verify: the writing steps claimed work, these bounded
/// reads say whether the machine holds it. Green names the check in
/// `verified`; red turns the check into a needs_human row, and the marker
/// gate upstream withholds on any needs_human, so a red verify never
/// stamps the run done. Exit stays 0 - the report is the verdict.
fn run_verify(exe: &Path, loaded: &[String], rep: &mut Report) {
    fn fold(rep: &mut Report, name: &str, result: Result<String, String>) {
        let Err(detail) = result else {
            rep.verified.push(name.to_string());
            return;
        };
        let message = format!("verify: {name} failed ({})", verify_reason(name, &detail));
        rep.rows
            .push(row("needs_human", "verify_red", message.clone()));
        rep.needs_human.push(message);
    }
    const VERIFY_CAP: std::time::Duration = std::time::Duration::from_secs(10);
    if let Some(exe) = exe.to_str() {
        fold(
            rep,
            "fno doctor --blockers",
            crate::setup_detect::bounded_for(&[exe, "doctor", "--blockers"], VERIFY_CAP),
        );
        fold(
            rep,
            "fno config doctor",
            crate::setup_detect::bounded_for(&[exe, "config", "doctor"], VERIFY_CAP),
        );
    } else {
        fold(
            rep,
            "fno doctor --blockers",
            Err("executable path is not valid UTF-8".into()),
        );
        fold(
            rep,
            "fno config doctor",
            Err("executable path is not valid UTF-8".into()),
        );
    }
    let serving = crate::digest_overlay::fno_agents_bin()
        .to_str()
        .and_then(|bin| {
            crate::setup_detect::bounded_for(&[bin, "status", "--json"], VERIFY_CAP).ok()
        })
        .and_then(|out| serde_json::from_str::<serde_json::Value>(out.trim()).ok())
        .and_then(|v| {
            v.get("daemon")
                .and_then(|d| d.get("state"))
                .and_then(serde_json::Value::as_str)
                .map(|s| s == "serving")
        })
        .unwrap_or(false);
    fold(
        rep,
        "fno-agents daemon serving",
        if serving {
            Ok(String::new())
        } else {
            Err("daemon is not serving".into())
        },
    );
    if loaded.is_empty() {
        return;
    }
    let labels = crate::setup_services::loaded_labels();
    for label in loaded {
        let ok = labels
            .as_ref()
            .map(|ls| ls.iter().any(|l| l == label))
            .unwrap_or(false);
        fold(
            rep,
            &format!("launchd {label}"),
            if ok {
                Ok(String::new())
            } else {
                Err("service is not loaded".into())
            },
        );
    }
}

/// The steps this run executes: the scope's layers, plus the contributor
/// rows when the repo root is a footnote source checkout (the root, not
/// the cwd: setup launched from a subdirectory still sees the crates),
/// filtered by `--only`. An `--only` id that names no selected step
/// refuses with the valid ids. The join step rides only a `--join` run:
/// it is out of the default pool, so a plain `--yes` never applies a
/// profile, and `--only join` without a file refuses here.
fn select_steps<'a>(opts: &'a Opts, root: &Path) -> Result<Vec<&'a Step>, String> {
    if opts.only.iter().any(|id| id == "join") && opts.join.is_none() {
        return Err("join needs --join <file or ->; export writes a profile".to_string());
    }
    let layers = |l: Layer| match opts.scope {
        Scope::Global => l == Layer::Global,
        Scope::Project => l == Layer::Project,
        Scope::Both => l != Layer::Contributor,
    };
    let mut steps: Vec<&Step> = STEPS
        .iter()
        .filter(|s| {
            layers(s.layer)
                || (opts.scope != Scope::Global
                    && s.layer == Layer::Contributor
                    && in_source_checkout(root))
        })
        .filter(|s| s.id != "join" || opts.join.is_some())
        .collect();
    if !opts.only.is_empty() {
        for id in &opts.only {
            if !steps.iter().any(|s| s.id == id) {
                let valid: Vec<&str> = STEPS.iter().map(|s| s.id).collect();
                return Err(format!(
                    "--only: unknown step id {id} for this scope; valid ids: {}",
                    valid.join(", ")
                ));
            }
        }
        steps.retain(|s| opts.only.iter().any(|id| id == s.id));
    }
    Ok(steps)
}

/// The dry-run envelope: the seven keys the plan pins, assembled pure of
/// writes. `services` is lifted out before `detected` moves into the
/// literal, because `json!` evaluates its fields in order and a moved
/// binding cannot be read by a later field.
fn dry_run_json(
    detected: serde_json::Value,
    proposed: &[Proposal],
    rows: &[Row],
    needs_human: &[String],
    fallback: &str,
) -> serde_json::Value {
    let services = detected
        .get("services")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let key_gap = proposed.iter().filter(|p| !p.key.is_empty()).count();
    json!({
        "detected": detected,
        "proposed": proposed.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
        "services": services,
        "needs_human": needs_human,
        "key_gap": key_gap,
        "rows": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        // The field the report contract carries in every mode; a dry run
        // verifies nothing, so it stays empty here.
        "verified": [],
        "next_action": next_action(rows, fallback),
    })
}

/// `--dry-run`: the read-only preview of the same step table. Detection
/// runs once; the gh rows fold off what detection already found, so a
/// dry-run never re-probes. It prints one JSON object through `redact`
/// whether or not `--json` passed, writes nothing, takes no marker, and
/// has no in-repo gate, so a fresh HOME answers too. `join_proposed`
/// carries the proposals run() built from the loaded profile, `pending`
/// the died-mid-join row when the in-progress flag is present.
fn dry_run(
    opts: &Opts,
    steps: &[&Step],
    root: &Path,
    join_proposed: &[Proposal],
    pending: Option<Row>,
) -> i32 {
    let scan: Option<Vec<PathBuf>> = if !opts.scan {
        None
    } else {
        Some(opts.scan_dirs.iter().map(PathBuf::from).collect())
    };
    let detected = crate::setup_detect::detect(root, scan.as_deref());
    let mut skips: Vec<String> = Vec::new();
    let mut rows: Vec<Row> = pending.into_iter().collect();
    let mut needs_human: Vec<String> = Vec::new();
    let mut proposed = compute_proposals(steps, root, &mut skips);
    proposed.extend_from_slice(join_proposed);
    match detected
        .get("gh")
        .and_then(|g| g.get("status"))
        .and_then(serde_json::Value::as_str)
    {
        Some("unreadable") => {
            let error = detected["gh"]["error"].as_str().unwrap_or_default();
            let message = format!("gh-auth: gh itself is missing or unreadable ({error})");
            rows.push(row("needs_human", "gh_unreadable", message.clone()));
            needs_human.push(message);
        }
        Some("ok") => {
            let missing: Vec<String> = detected["gh"]["missing"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !missing.is_empty() {
                let message = format!(
                    "gh-auth: missing scopes {} - run `gh auth refresh -s repo -s workflow`",
                    missing.join(", ")
                );
                rows.push(row("needs_human", "gh_scope_missing", message.clone()));
                needs_human.push(message);
            }
        }
        _ => {}
    }
    let fallback = "fno config setup run --yes";
    let body = dry_run_json(detected, &proposed, &rows, &needs_human, fallback);
    println!(
        "{}",
        serde_json::to_string_pretty(&crate::setup_profile::redact(&body)).unwrap_or_default()
    );
    0
}

/// The proposal half of a dry run, shared with `--check`: the same rows a
/// dry run offers, computed with nothing printed. `skips` collects the
/// "why not" lines `--dry-run` prints.
fn compute_proposals(steps: &[&Step], root: &Path, skips: &mut Vec<String>) -> Vec<Proposal> {
    let mut proposed: Vec<Proposal> = Vec::new();
    for step in steps {
        match step.id {
            "harness-wiring" => proposed.push(propose_harness_wiring()),
            "concurrency-band" => {
                if let Some(n) = propose_concurrency_band(skips) {
                    proposed.push(Proposal {
                        id: "concurrency-band",
                        key: "agents.max_live".into(),
                        value: n.to_string(),
                        scope: "global",
                        reason: "the machine sample's measured band, so live agents stay inside it"
                            .to_string(),
                        source: "sample",
                    });
                }
            }
            "guardrails" => {
                if let Some(p) = propose_guardrails(skips) {
                    proposed.push(p);
                }
            }
            "backlog-prefix" => {
                if let Some(p) = propose_backlog_prefix(root, skips) {
                    proposed.push(p);
                }
            }
            _ => {}
        }
    }
    proposed
}

/// One proposal value from its stored text. Some proposals hold bare words
/// ("standard", "fn"), so the first read takes the token as TOML gives it
/// and the second quotes it the way Rust spells a string. A token neither
/// form reads is None, and the caller reports drift rather than guessing.
fn proposal_token(token: &str) -> Option<toml::Value> {
    if let Ok(v) = toml::from_str::<toml::Value>(&format!("v = {token}")) {
        return v.get("v").cloned();
    }
    toml::from_str::<toml::Value>(&format!("v = {token:?}"))
        .ok()
        .and_then(|v| v.get("v").cloned())
}

/// A proposal value as the drift lines name it: strings bare, everything
/// else its TOML form.
fn value_word(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The step event's outcome from the report slices this pass appended to:
/// an explicit skip wins, then progress on done, else a step that ran
/// nothing and blocked.
fn step_outcome<'a>(
    id: &str,
    new_done: &[String],
    new_skipped: &'a [String],
) -> (&'static str, Option<&'a str>) {
    if let Some(entry) = new_skipped.last() {
        let prefix = format!("{id}: ");
        (
            "skipped",
            Some(entry.strip_prefix(prefix.as_str()).unwrap_or(entry)),
        )
    } else if !new_done.is_empty() {
        ("done", None)
    } else {
        ("blocked", None)
    }
}

/// The first line's trailing 20-byte UTC stamp, the bytes `write_marker`
/// keeps stable across rewrites. None when the marker is absent or its
/// first line is too short or splits inside the stamp.
fn marker_first_stamp(path: &Path) -> Option<String> {
    let first = std::fs::read_to_string(path)
        .ok()?
        .lines()
        .next()?
        .to_string();
    let n = first.len();
    if n < 20 || !first.is_char_boundary(n - 20) {
        return None;
    }
    let stamp = &first[n - 20..];
    stamp.ends_with('Z').then(|| stamp.to_string())
}

/// The first merge since the machine's first run, one bounded `gh` read.
/// ISO stamps sort lexically, so the minimum mergedAt is the first. A
/// missing marker, no repo, no gh auth, or a timeout all answer None.
fn first_merged_pr_at(first_run_at: Option<&str>) -> Option<String> {
    let first_run_at = first_run_at?;
    let search = format!("merged:>={first_run_at}");
    let argv: Vec<&str> = vec![
        "gh",
        "pr",
        "list",
        "--state",
        "merged",
        "--author",
        "@me",
        "--search",
        search.as_str(),
        "--json",
        "mergedAt",
        "--limit",
        "20",
    ];
    let out = crate::setup_detect::bounded_for(&argv, std::time::Duration::from_secs(5)).ok()?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(out.trim()).ok()?;
    rows.iter()
        .filter_map(|r| r.get("mergedAt").and_then(serde_json::Value::as_str))
        .min()
        .map(String::from)
}

/// Best-effort journal append for setup's own events: one stderr line on
/// failure, never a failed setup. No config key and no value rides in
/// `data`; callers pass ids, outcomes and durations only.
pub(crate) fn append_setup_event(kind: &str, data: serde_json::Value) {
    let envelope = json!({
        "ts": utc_now(),
        "type": kind,
        "source": "cli",
        "data": data,
    });
    let path = crate::review_invocation::review_events_path();
    if let Err(e) = crate::event_store::append_envelope(&path, &envelope.to_string(), None) {
        eprintln!("setup: event append failed: {e}");
    }
}

/// `--check`: recompute the proposal `--dry-run` computes, diff every row
/// against the live config, diff the sidecar profile when one exists, run
/// the verify block, and print one fix line per drifted row. It writes
/// nothing; its exit code is the verdict.
fn run_check(opts: &Opts, steps: &[&Step], root: &Path, join_proposed: &[Proposal]) -> i32 {
    let mut skips: Vec<String> = Vec::new();
    let mut proposed = compute_proposals(steps, root, &mut skips);
    proposed.extend_from_slice(join_proposed);
    let global = crate::config_defaults::global_config_path();
    let project = root.join(".fno").join("config.toml");
    let mut drift: Vec<String> = Vec::new();
    drift.extend(prerequisite_lines());
    for p in &proposed {
        if p.key.is_empty() {
            continue;
        }
        let Some(want) = proposal_token(&p.value) else {
            drift.push(format!(
                "drift: {} holds a value setup cannot parse - fix: fno config setup run --yes --only {}",
                p.key, p.id
            ));
            continue;
        };
        let have = crate::config_defaults::lookup_value(Some(&global), Some(&project), &p.key);
        if have.as_ref() != Some(&want) {
            let have_word = have
                .map(|v| value_word(&v))
                .unwrap_or_else(|| "unset".into());
            drift.push(format!(
                "drift: {} is {} (proposed {}) - fix: fno config setup run --yes --only {}",
                p.key,
                have_word,
                value_word(&want),
                p.id
            ));
        }
    }
    if crate::setup_services::macos() {
        let services = [
            (
                "pr-watcher",
                "pr_watch.enabled",
                crate::setup_services::PR_WATCHER_LABEL,
            ),
            (
                "auto-heal",
                "auto_heal.enabled",
                crate::setup_services::PR_WATCHER_LABEL,
            ),
            ("groom-agent", "", crate::setup_services::GROOM_LABEL),
        ];
        for (id, key, label) in services {
            if !steps.iter().any(|s| s.id == id) {
                continue;
            }
            let configured = key.is_empty()
                || crate::config_defaults::lookup_value(Some(&global), Some(&project), key)
                    .as_ref()
                    == Some(&toml::Value::Boolean(true));
            if !configured {
                drift.push(format!(
                    "drift: {id} is not enabled - fix: fno config setup run --yes --only {id}"
                ));
            } else if !crate::setup_services::is_loaded(label) {
                drift.push(format!(
                    "drift: {label} is not loaded - fix: fno config setup run --yes --only {id}"
                ));
            }
        }
    }
    let profile_path = crate::setup_profile::profile_path();
    if profile_path.exists() {
        match std::fs::read_to_string(&profile_path)
            .map_err(|e| e.to_string())
            .and_then(|text| crate::setup_profile::parse_profile(&text))
        {
            Err(e) => drift.push(format!(
                "drift: the sidecar profile at {} does not read ({e}) - fix: fno config setup run --yes --only join",
                profile_path.display()
            )),
            Ok(profile) => {
                for (key, value) in &profile.config {
                    if proposed.iter().any(|p| p.key == key.as_str()) {
                        continue;
                    }
                    if !matches!(
                        crate::setup_profile::classify(key, &toml_as_json(value)),
                        crate::setup_profile::Class::Portable
                    ) {
                        continue;
                    }
                    if crate::config_defaults::lookup_value(Some(&global), Some(&project), key)
                        .as_ref()
                        != Some(value)
                    {
                        drift.push(format!(
                            "drift: {key} differs from the sidecar profile - fix: fno config setup run --yes --only join"
                        ));
                    }
                }
            }
        }
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno"));
    let mut rep = Report::default();
    run_verify(&exe, &[], &mut rep);
    let red: Vec<&Row> = rep.rows.iter().filter(|r| r.code == "verify_red").collect();
    let first_run_at =
        marker_first_stamp(&global_marker()).or_else(|| marker_first_stamp(&project_marker(root)));
    let first_merged = first_merged_pr_at(first_run_at.as_deref());
    let key_gap = drift.len() + red.len();
    if opts.json {
        let body = json!({
            "first_run_at": first_run_at,
            "key_gap": key_gap,
            "first_merged_pr_at": first_merged,
            "drift": drift,
            "rows": red.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&crate::setup_profile::redact(&body)).unwrap_or_default()
        );
        return if key_gap == 0 { 0 } else { 1 };
    }
    for line in &drift {
        println!("{line}");
    }
    for r in &red {
        println!("{}", r.message);
    }
    if key_gap == 0 {
        println!("setup: no drift");
        0
    } else {
        1
    }
}

/// The pieces a clean machine lacks before setup can help: one line each,
/// in the `drift:` shape, with the command that fixes it. They count toward
/// `--check`'s exit the way drift does.
fn prerequisite_lines() -> Vec<String> {
    let detected = crate::setup_detect::detect(Path::new("."), None);
    let mut rows = Vec::new();
    let any_harness = detected["harnesses"]
        .as_array()
        .is_some_and(|items| items.iter().any(|h| h["on_path"] == true));
    if !any_harness {
        rows.push(
            "missing: agent CLI - fix: install Claude Code (https://claude.com/download) or Codex (https://developers.openai.com/codex/cli)"
                .to_string(),
        );
    }
    let gh = &detected["gh"];
    if gh["status"] == "unreadable" {
        rows.push(
            "missing: GitHub CLI login - fix: `brew install gh`, then `gh auth login`".into(),
        );
    } else if gh["missing"]
        .as_array()
        .is_some_and(|missing| !missing.is_empty())
    {
        rows.push("missing: GitHub CLI scopes repo, workflow - fix: `gh auth refresh -s repo -s workflow`".into());
    }
    if detected["tools"]["jq"] != true {
        rows.push("missing: jq - fix: `brew install jq`".into());
    }
    match crate::setup_detect::bounded_output(&["git", "--version"]) {
        Ok(version) if git_version_at_least(&version, (2, 31)) => {}
        Ok(version) => rows.push(format!(
            "outdated: {} (need 2.31 or newer) - fix: `brew install git`",
            version.trim()
        )),
        Err(_) => rows.push("missing: git - fix: `brew install git`".into()),
    }
    match crate::digest_overlay::fno_agents_bin_auto() {
        None => rows.push("missing: fno-agents - fix: `curl -fsSL fno.sh | sh`".into()),
        Some(agents) => {
            let dir = agents.parent().unwrap_or(Path::new("."));
            for stem in ["fno-agents-daemon", "fno-agents-worker"] {
                if !dir.join(stem).is_file() {
                    rows.push(format!(
                        "missing: {stem} beside {} - fix: `curl -fsSL fno.sh | sh`",
                        agents.display()
                    ));
                }
            }
        }
    }
    rows
}

fn git_version_at_least(text: &str, minimum: (u32, u32)) -> bool {
    let Some(version) = text.trim().strip_prefix("git version ") else {
        return false;
    };
    let mut parts = version
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => (major, minor) >= minimum,
        _ => false,
    }
}

/// `--list`: the step table the chat skill asks from. `--list --json` is
/// the machine shape; plain `--list` prints one row per step.
fn print_list(opts: &Opts) {
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&list_json()).unwrap_or_default()
        );
        return;
    }
    for s in STEPS {
        println!(
            "{:<18} {:<12} {:<8} {}",
            s.id,
            layer_name(s.layer),
            kind_name(s.kind),
            s.question
        );
    }
}

/// The `--list --json` rows. The messaging row also carries each chat
/// channel and whether this machine has it configured.
pub(crate) fn list_json() -> serde_json::Value {
    let arr: Vec<serde_json::Value> = STEPS
        .iter()
        .map(|s| {
            let mut v = json!({
                "id": s.id,
                "layer": layer_name(s.layer),
                "kind": kind_name(s.kind),
                "question": s.question,
                "effect": s.effect,
            });
            if s.id == "messaging" {
                match crate::setup_messaging::sink_rows() {
                    Ok(rows) => {
                        let me = crate::setup_messaging::machine();
                        v["channels"] = crate::setup_messaging::channel_states(&rows, &me)
                            .iter()
                            .map(|(kind, on)| json!({"type": kind, "configured": on}))
                            .collect();
                    }
                    Err(e) => v["error"] = json!(e),
                }
            }
            v
        })
        .collect();
    serde_json::Value::Array(arr)
}

/// The messaging step: the terminal walk when prompting, else one skip
/// line naming the re-entry command. `picked` carries the wizard's picks.
fn step_messaging(exe: &Path, prompting: bool, picked: Option<&[&str]>, rep: &mut Report) {
    use crate::setup_messaging as m;
    let rows = match m::sink_rows() {
        Ok(rows) => rows,
        Err(e) => {
            let message = format!("messaging: {e}");
            rep.rows
                .push(row("needs_human", "messaging_unfinished", message.clone()));
            rep.needs_human.push(message);
            return;
        }
    };
    if !prompting {
        rep.skipped.push(m::summary_line(&rows, &m::machine()));
        return;
    }
    let set = |value: &str| config_set(exe, &[("attention.sinks", value)], false);
    let home = match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home),
        None => {
            let message = "messaging: cannot write ~/.fno/.env: HOME is unset".to_string();
            rep.rows
                .push(row("needs_human", "messaging_unfinished", message.clone()));
            rep.needs_human.push(message);
            return;
        }
    };
    let out = m::step(
        rows,
        picked,
        &mut m::TermAsk::new(),
        &set,
        &mut m::Curl,
        &home,
    );
    rep.done.extend(out.done);
    rep.skipped.extend(out.skipped);
    rep.paths.extend(out.paths);
    for message in out.needs_human {
        rep.rows
            .push(row("needs_human", "messaging_unfinished", message.clone()));
        rep.needs_human.push(message);
    }
}

fn layer_name(l: Layer) -> &'static str {
    match l {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Contributor => "contributor",
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Act => "act",
        Kind::Approve => "approve",
        Kind::Report => "report",
        Kind::Human => "human",
    }
}

/// Whether every layer this scope covers already carries its done marker.
fn all_markers_present(scope: Scope, root: &Path) -> bool {
    all_markers_present_in(
        scope,
        global_marker().exists(),
        project_marker(root).exists(),
    )
}

/// The pure core of [`all_markers_present`], so the per-layer conditions are
/// unit-testable without touching a real state dir: a layer outside this
/// scope never blocks, a covered layer needs its own marker.
fn all_markers_present_in(scope: Scope, global_done: bool, project_done: bool) -> bool {
    let global_ok = scope == Scope::Project || global_done;
    let project_ok = scope == Scope::Global || project_done;
    global_ok && project_ok
}

/// Write a layer's done marker (version + UTC stamp), parent dirs included.
/// The first line is the original first-run stamp: a later write appends a
/// fresh line and keeps it, so `--check`'s first_run_at stays the first run.
fn write_marker(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let first = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().next().map(str::to_string))
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| marker_line());
    let _ = std::fs::write(path, format!("{first}\n{}\n", marker_line()));
}

/// Ask one yes/no question on the terminal, recommended default yes. EOF
/// reads as a decline so a closing pipe never wedges the run.
fn ask(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => false,
        Ok(_) => {
            let a = line.trim().to_lowercase();
            a.is_empty() || a == "y" || a == "yes"
        }
        Err(_) => false,
    }
}

/// The wizard's apply pass: the proposal rows the flow confirmed, applied
/// once after the conversation ends. Keyed rows go through `config_set`
/// with the row's committed value - the run's own band step re-detects the
/// number and the review-merge step has no source but a wizard answer -
/// while service and project rows call the same runners `run` uses, so
/// installs, keys and verify behave identically. The loaded set starts
/// empty and the runners seed it, exactly as a run does. Join emits no
/// rows here; its writes happen in the flow's join step. Returns the
/// redacted report JSON for the wizard's own report.
pub(crate) fn apply_wizard_pending(
    exe: &Path,
    root: &Path,
    rid: &str,
    rows: &[Proposal],
) -> serde_json::Value {
    let started = std::time::Instant::now();
    let mut rep = Report::default();
    rep.run_id = rid.to_string();
    rep.host = "wizard";
    let mut loaded: Vec<String> = Vec::new();
    for p in rows {
        let step_started = std::time::Instant::now();
        let done_len = rep.done.len();
        let skipped_len = rep.skipped.len();
        let rows_len = rep.rows.len();
        match p.id {
            "concurrency-band" => {
                let pairs = [("agents.max_live", p.value.as_str())];
                match config_set(exe, &pairs, false) {
                    Ok(path) => {
                        rep.done
                            .push(format!("concurrency-band: agents.max_live = {}", p.value));
                        rep.paths.push(path.display().to_string());
                        rep.restart_needed = true;
                    }
                    Err(e) => {
                        let message = format!("concurrency-band: {e}");
                        rep.rows
                            .push(row("needs_human", "config_set_refused", message.clone()));
                        rep.needs_human.push(message);
                    }
                }
            }
            "review-merge" => {
                let pairs = [(p.key.as_str(), p.value.as_str())];
                match config_set(exe, &pairs, false) {
                    Ok(path) => {
                        rep.done
                            .push(format!("review-merge: {} = {}", p.key, p.value));
                        rep.paths.push(path.display().to_string());
                    }
                    Err(e) => {
                        let message = format!("review-merge: {e}");
                        rep.rows
                            .push(row("needs_human", "config_set_refused", message.clone()));
                        rep.needs_human.push(message);
                    }
                }
            }
            "pr-watcher" => step_pr_watcher(exe, &mut rep, &mut loaded),
            "auto-heal" => step_auto_heal(exe, &mut rep),
            "groom-agent" => step_groom_agent(exe, &mut rep, &mut loaded),
            "projects" => step_projects(exe, root, &mut rep),
            "messaging" => step_messaging(exe, true, Some(&[p.value.as_str()]), &mut rep),
            _ => rep.skipped.push(format!("{}: no runner", p.id)),
        }
        let outcome = step_outcome(p.id, &rep.done[done_len..], &rep.skipped[skipped_len..]);
        let mut data = json!({
            "run_id": rid,
            "step": p.id,
            "outcome": outcome.0,
            "duration_ms": step_started.elapsed().as_millis() as u64,
        });
        if let Some(reason) = outcome.1 {
            data["skip_reason"] = json!(reason);
        }
        // A step can push several rows; name every new code so the event
        // never under-reports the step's failure shape.
        let codes: Vec<&str> = rep.rows[rows_len..].iter().map(|r| r.code).collect();
        if !codes.is_empty() {
            data["code"] = json!(codes.join(","));
        }
        append_setup_event("setup_step", data);
    }
    run_verify(exe, &loaded, &mut rep);
    append_setup_event(
        "setup_run",
        json!({
            "run_id": rid,
            "host": rep.host,
            "outcome": if rep.needs_human.is_empty() {
                "finished"
            } else {
                "blocked"
            },
            "duration_ms": started.elapsed().as_millis() as u64,
        }),
    );
    rep.next_action = next_action(&rep.rows, "/fno:target \"<your first feature>\"");
    crate::setup_profile::redact(&rep.to_json())
}

/// The wizard's join lane. `step_join` already backs up, marks the
/// in-progress flag and restores on a refused write; the wrapper only
/// gives the report the wizard's host and run id, journals the lane's
/// `setup_run` event, and returns the redacted JSON for the wizard's own
/// report. Profile rows never pass through `pending`: a joined machine's
/// changes are the profile's, not the flow's answers.
pub(crate) fn apply_wizard_join(
    exe: &Path,
    profile: &crate::setup_profile::Profile,
    rows: &[crate::setup_profile::DiffRow],
    rid: &str,
) -> serde_json::Value {
    let started = std::time::Instant::now();
    let mut rep = Report::default();
    rep.run_id = rid.to_string();
    rep.host = "wizard";
    step_join(exe, profile, rows, &mut rep);
    append_setup_event(
        "setup_run",
        json!({
            "run_id": rid,
            "host": rep.host,
            "lane": "join",
            "outcome": if rep.needs_human.is_empty() {
                "finished"
            } else {
                "blocked"
            },
            "duration_ms": started.elapsed().as_millis() as u64,
        }),
    );
    rep.next_action = if rep.needs_human.is_empty() {
        "/fno:target \"<your first feature>\"".to_string()
    } else {
        "fix the needs-human rows above, then rerun `fno do wizard`".to_string()
    };
    crate::setup_profile::redact(&rep.to_json())
}

/// The command. `--list` prints the table; `--once` stops at its markers; the
/// no-prompt path takes every recommended default. A layer's marker is
/// written only when that layer left nothing needs-human, so `--once` never
/// papers over a blocked step. Exit 0 unless the argv was wrong.
pub fn run(tail: &[OsString]) -> i32 {
    // parse_opts has not run yet, so --json is read straight off the tail:
    // a --json caller gets every refusal as one JSON object on stdout,
    // never prose on stderr.
    let json_wanted = tail
        .iter()
        .filter_map(|a| a.to_str())
        .any(|a| a == "--json" || a == "-J");
    let refuse = |e: String| {
        if json_wanted {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::setup_profile::redact(&refused_json(
                    &format!("fno config setup run: {e}")
                )))
                .unwrap_or_default()
            );
        } else {
            eprintln!("fno config setup run: {e}");
        }
        EXIT_USAGE
    };
    let opts = match parse_opts(tail) {
        Ok(o) => o,
        Err(e) => return refuse(e),
    };
    if opts.help {
        println!("{USAGE}");
        return 0;
    }
    if opts.export && opts.join.is_some() {
        return refuse(
            "--export and --join are exclusive: export writes a profile, join reads one".into(),
        );
    }
    if opts.out.is_some() && !opts.export {
        return refuse("--out only matters with --export".into());
    }
    if opts.list {
        print_list(&opts);
        return 0;
    }
    // A previous join that died mid-write heals before anything reads the
    // global config, export included: restore its backup, then report.
    // A dry run never mutates, so it skips the heal and reports the
    // in-progress flag as a pending row instead.
    let healed = if opts.dry_run {
        None
    } else {
        crate::setup_profile::heal()
    };
    // Export is a flag-level mode, not a step: it reads only the global
    // file and exits. `--only` and `--dry-run` do not reach it, so a
    // profile can be cut on a fully-set-up machine and piped anywhere.
    if opts.export {
        if let Some((_, msg)) = &healed {
            eprintln!("join: {msg}");
        }
        let global = crate::config_defaults::global_config_path();
        let flat = match crate::config_defaults::read_flat(&global) {
            Err(e) => return refuse(format!("--export: the global config does not parse: {e}")),
            Ok(None) => std::collections::BTreeMap::new(),
            Ok(Some(map)) => map,
        };
        let exported = crate::setup_profile::export(&flat);
        eprintln!(
            "export: {} machine-local key(s) stay local, {} secret(s) stay behind",
            exported.kept_local.len(),
            exported.secrets
        );
        match &opts.out {
            Some(path) => match std::fs::write(path, &exported.text) {
                Ok(()) => println!("export: wrote {path}"),
                Err(e) => return refuse(format!("--out {path}: cannot write: {e}")),
            },
            None => print!("{}", exported.text),
        }
        return 0;
    }
    let root = repo_root();
    // Validate the argv (including --only) before --once can return early.
    let steps = match select_steps(&opts, &root) {
        Ok(s) => s,
        Err(e) => return refuse(e),
    };
    // The join lane: the profile is validated up front, so a bad file
    // refuses before anything prints or writes. The diff banner always
    // prints, even on a full run where join only rides in `proposed`:
    // the operator sees what a join would change before approving it.
    let profile = match &opts.join {
        None => None,
        Some(path) => match crate::setup_profile::load_profile(path) {
            Ok(p) => Some(p),
            Err(e) => return refuse(format!("--join {path}: {e}")),
        },
    };
    let (diff_rows, join_proposed) = match &profile {
        None => (Vec::new(), Vec::new()),
        Some(p) => {
            let here = match crate::setup_profile::here_config() {
                Ok(h) => h,
                Err(e) => return refuse(format!("--join: the global config does not parse: {e}")),
            };
            let (rows, secrets) = crate::setup_profile::diff(p, &here);
            // JSON stdout stays parseable: the human preview rides stderr.
            let banner = crate::setup_profile::format_diff(&rows, secrets, &p.host, &p.exported_at);
            if opts.json {
                eprintln!("{banner}");
            } else {
                println!("{banner}");
            }
            let proposed: Vec<Proposal> = rows
                .iter()
                .filter(|r| r.applies())
                .map(|r| Proposal {
                    id: "join",
                    key: r.key.clone(),
                    value: crate::setup_profile::toml_inline(
                        r.apply.as_ref().expect("apply rows carry a value"),
                    ),
                    scope: "global",
                    reason: format!("join: {}", r.kind),
                    source: "profile",
                })
                .collect();
            (rows, proposed)
        }
    };
    // The read-only preview runs before any --once gate: a dry run reports
    // even on an already-set-up machine, and writes nothing, marker included.
    if opts.dry_run {
        let pending = if crate::setup_profile::in_progress_flag().exists() {
            Some(row(
                "needs_human",
                "join_pending",
                format!(
                    "a previous join died mid-write; inspect {} before joining again",
                    crate::setup_profile::backup_dir().display()
                ),
            ))
        } else {
            None
        };
        return dry_run(&opts, &steps, &root, &join_proposed, pending);
    }
    // `--check` runs past the --once gate on purpose: it reports drift on a
    // marker-complete machine too, and writes nothing.
    if opts.check {
        return run_check(&opts, &steps, &root, &join_proposed);
    }
    if opts.once && all_markers_present(opts.scope, &root) {
        let msg = format!("setup: already done ({})", global_marker().display());
        if opts.json {
            let mut rep = Report::default();
            rep.run_id = run_id();
            rep.next_action = "/fno:target \"<your first feature>\"".to_string();
            rep.skipped.push(msg);
            println!("{}", rep.to_json());
        } else {
            println!("{msg}");
        }
        return 0;
    }
    // The project and contributor layers answer only inside a git repo:
    // repo_root() falls back to the cwd, and setup must not litter an
    // arbitrary directory with .fno/.
    let in_repo = root.join(".git").exists();
    // A JSON stdout must stay pure: an interactive TTY never prompts into it.
    let prompting = !opts.json && !no_prompt(opts.yes, std::io::stdin().is_terminal(), agent_env());
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno"));
    let run_started = std::time::Instant::now();
    let mut rep = Report::default();
    rep.run_id = run_id();
    let rid = rep.run_id.clone();
    match healed {
        Some((true, msg)) => rep.done.push(format!("join: {msg}")),
        Some((false, msg)) => {
            let message = format!("join: {msg}");
            rep.rows
                .push(row("needs_human", "join_pending", message.clone()));
            rep.needs_human.push(message);
        }
        None => {}
    }
    if !in_repo && opts.scope != Scope::Global {
        rep.skipped.push(
            "project layer: not a git repo; the project and contributor steps need one".into(),
        );
    }
    if prompting && !opts.json {
        println!("fno config setup run - Enter accepts each recommended default.");
    }
    let mut declined = 0usize;
    let mut loaded: Vec<String> = Vec::new();
    for step in &steps {
        let step_started = std::time::Instant::now();
        let done_len = rep.done.len();
        let skipped_len = rep.skipped.len();
        let rows_len = rep.rows.len();
        if !in_repo && step.layer != Layer::Global {
            append_setup_event(
                "setup_step",
                json!({
                    "run_id": rid.as_str(),
                    "step": step.id,
                    "outcome": "skipped",
                    "duration_ms": step_started.elapsed().as_millis() as u64,
                    "skip_reason": "not a git repo",
                }),
            );
            continue;
        }
        // An approve step never runs bare: --yes leaves it in the report's
        // proposed list with its exact command, and it executes only when
        // --only names its id. It does not block the marker - the step did
        // not fail, it waits for a caller that asks for it.
        if step.kind == Kind::Approve && !opts.only.iter().any(|id| id == step.id) {
            rep.proposed.push(Proposal {
                id: step.id,
                key: "".into(),
                value: String::new(),
                scope: layer_name(step.layer),
                reason: step.effect.to_string(),
                source: "default",
            });
            append_setup_event(
                "setup_step",
                json!({
                    "run_id": rid.as_str(),
                    "step": step.id,
                    "outcome": "skipped",
                    "duration_ms": step_started.elapsed().as_millis() as u64,
                    "skip_reason": "awaits an explicit --only",
                }),
            );
            continue;
        }
        let go = match step.kind {
            Kind::Act if prompting => {
                let q = format!("Install {}? It will {}. [Y/n]", step.id, step.effect);
                ask(&q)
            }
            Kind::Act => true,
            _ => true,
        };
        if !go {
            rep.skipped.push(format!("{}: declined", step.id));
            declined += 1;
            append_setup_event(
                "setup_step",
                json!({
                    "run_id": rid.as_str(),
                    "step": step.id,
                    "outcome": "skipped",
                    "duration_ms": step_started.elapsed().as_millis() as u64,
                    "skip_reason": "declined",
                }),
            );
            continue;
        }
        match (step.layer, step.id) {
            (Layer::Global, "harness-wiring") => {
                let out = std::process::Command::new(&exe)
                    .args(["config", "setup", "auto-wire"])
                    .stdin(std::process::Stdio::null())
                    .output();
                match out {
                    Ok(o) => {
                        for line in String::from_utf8_lossy(&o.stdout).lines() {
                            fold_wire_line(line, &mut rep);
                        }
                    }
                    Err(e) => rep.needs_human.push(format!(
                        "harness-wiring: cannot run the wiring command ({e})"
                    )),
                }
            }
            (Layer::Global, "gh-auth") => step_gh_auth(&mut rep),
            (Layer::Global, "concurrency-band") => step_concurrency_band(&exe, &mut rep),
            (Layer::Global, "accounts") => rep
                .skipped
                .push("accounts: review with `fno config accounts`; login needs a browser".into()),
            (Layer::Global, "guardrails") => step_guardrails(&exe, &mut rep),
            (Layer::Global, "pr-watcher") => step_pr_watcher(&exe, &mut rep, &mut loaded),
            (Layer::Global, "auto-heal") => step_auto_heal(&exe, &mut rep),
            (Layer::Global, "groom-agent") => step_groom_agent(&exe, &mut rep, &mut loaded),
            (Layer::Global, "projects") => step_projects(&exe, &root, &mut rep),
            (Layer::Global, "review-merge") => step_review_merge(&mut rep),
            (Layer::Global, "messaging") => step_messaging(&exe, prompting, None, &mut rep),
            (Layer::Global, "machine-tools") => step_machine_tools(&mut rep),
            (Layer::Global, "permission-rules") => step_permission_rules(&mut rep),
            (Layer::Global, "join") => {
                if let Some(p) = &profile {
                    step_join(&exe, p, &diff_rows, &mut rep);
                }
            }
            (Layer::Project, "worktree-policy") => rep
                .skipped
                .push("worktree-policy: left unset (harness-native default)".into()),
            (Layer::Project, "plans-dir") => rep
                .skipped
                .push("plans-dir: plans_dir stays at its default (.fno/plans/)".into()),
            (Layer::Project, "review-bots") => rep.skipped.push(
                "review-bots: review.github_apps left empty (no bot reviews requested)".into(),
            ),
            (Layer::Project, "backlog-prefix") => step_backlog_prefix(&exe, &root, &mut rep),
            (Layer::Contributor, "cargo-toolchain") => {
                let v = std::process::Command::new("cargo")
                    .arg("--version")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                rep.skipped.push(format!(
                    "cargo-toolchain: {}",
                    v.unwrap_or_else(
                        || "install rustup: curl https://sh.rustup.rs -sSf | sh".into()
                    )
                ));
            }
            (Layer::Contributor, "crates-build") => rep.skipped.push(format!(
                "crates-build: run `cargo build --locked` at {}",
                root.display()
            )),
            (Layer::Contributor, "cargo-bin-on-path") => {
                let on = std::env::var_os("PATH").unwrap_or_default();
                let hit = std::env::split_paths(&on).any(|d| d.ends_with(".cargo/bin"));
                rep.skipped.push(if hit {
                    "cargo-bin-on-path: ~/.cargo/bin is on PATH".into()
                } else {
                    "cargo-bin-on-path: add `export PATH=\"$HOME/.cargo/bin:$PATH\"` to your rc"
                        .into()
                });
            }
            _ => rep.skipped.push(format!("{}: no runner", step.id)),
        }
        let outcome = step_outcome(step.id, &rep.done[done_len..], &rep.skipped[skipped_len..]);
        let mut data = json!({
            "run_id": rid.as_str(),
            "step": step.id,
            "outcome": outcome.0,
            "duration_ms": step_started.elapsed().as_millis() as u64,
        });
        if let Some(reason) = outcome.1 {
            data["skip_reason"] = json!(reason);
        }
        // A step can push several rows; name every new code so the event
        // never under-reports the step's failure shape.
        let codes: Vec<&str> = rep.rows[rows_len..].iter().map(|r| r.code).collect();
        if !codes.is_empty() {
            data["code"] = json!(codes.join(","));
        }
        append_setup_event("setup_step", data);
    }
    run_verify(&exe, &loaded, &mut rep);
    append_setup_event(
        "setup_run",
        json!({
            "run_id": rid.as_str(),
            "host": rep.host,
            "outcome": if rep.needs_human.is_empty() && declined == 0 {
                "finished"
            } else {
                "blocked"
            },
            "duration_ms": run_started.elapsed().as_millis() as u64,
        }),
    );

    let global_ran = steps.iter().any(|s| s.layer == Layer::Global);
    let project_ran = steps.iter().any(|s| s.layer != Layer::Global && in_repo);
    // A layer's marker waits until nothing needs a human and nothing was
    // declined: `--once` never papers over a blocked or refused run. A
    // partial `--only` pass never writes one either - the steps it skipped
    // have not run, so a later full pass must still offer them. Step
    // runners are idempotent and differ-guarded, so the rerun is cheap.
    if opts.only.is_empty() && rep.needs_human.is_empty() && declined == 0 {
        if global_ran {
            write_marker(&global_marker());
        }
        if project_ran {
            write_marker(&project_marker(&root));
        }
    }
    if opts.json {
        rep.next_action = next_action(&rep.rows, "/fno:target \"<your first feature>\"");
        println!("{}", rep.to_json());
        return 0;
    }
    if !rep.done.is_empty() {
        println!("done:");
        for e in &rep.done {
            println!("  {e}");
        }
    }
    if !rep.skipped.is_empty() {
        println!("skipped:");
        for e in &rep.skipped {
            println!("  {e}");
        }
    }
    if !rep.needs_human.is_empty() {
        println!("needs a human:");
        for e in &rep.needs_human {
            println!("  {e}");
        }
    }
    if !rep.paths.is_empty() {
        println!("wrote:");
        for p in &rep.paths {
            println!("  {p}");
        }
    }
    if let Some(undo) = &rep.undo {
        println!("{undo}");
    }
    let markers = global_ran
        .then(|| global_marker().display().to_string())
        .into_iter()
        .chain(project_ran.then(|| project_marker(&root).display().to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    println!("markers: {markers}");
    if rep.restart_needed {
        println!("a restart of any running agent session picks up the new wiring.");
    }
    0
}

#[cfg(test)]
#[path = "setup_run_tests.rs"]
mod tests;
